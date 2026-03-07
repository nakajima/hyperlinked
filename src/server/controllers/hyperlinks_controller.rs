use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
};

use axum::{
    Json, Router,
    body::{self, Body},
    extract::{Form, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing,
};
use sailfish::Template;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};

use crate::{
    entity::{
        hyperlink::{self, HyperlinkSourceType},
        hyperlink_artifact::{self, HyperlinkArtifactKind},
        hyperlink_processing_job,
    },
    model::{
        hyperlink::HyperlinkInput, hyperlink_search_doc, hyperlink_tagging, hyperlink_title,
        tagging_settings,
    },
    server::{
        context::Context,
        flash::{Flash, FlashName, redirect_with_flash},
    },
};

use crate::server::{
    hyperlink_fetcher::{
        HyperlinkFetchQuery, HyperlinkFetchResults, HyperlinkFetcher, OrderToken, ScopeToken,
        StatusToken, TypeToken,
    },
    views,
};

const SHOW_TIMELINE_LIMIT: u64 = 10;
const HYPERLINKS_PATH: &str = "/hyperlinks";

pub fn links() -> Router<Context> {
    Router::new()
        .route("/hyperlinks", routing::get(index))
        .route("/hyperlinks/new", routing::get(new))
        .route("/hyperlinks.json", routing::get(index_json))
        .route("/hyperlinks/lookup", routing::get(lookup_url))
        .route("/hyperlinks", routing::post(create))
        .route("/hyperlinks.json", routing::post(create_json))
        .route("/hyperlinks/{id}/click", routing::post(click))
        .route("/hyperlinks/{id}/visit", routing::get(visit))
        .route("/hyperlinks/{id}/edit", routing::get(edit))
        .route("/hyperlinks/{id}/update", routing::post(update_html_post))
        .route("/hyperlinks/{id}/delete", routing::post(delete_html_post))
        .route("/hyperlinks/{id}/reprocess", routing::post(reprocess))
        .route(
            "/hyperlinks/{id}/tags",
            routing::post(update_tags_html_post),
        )
        .route(
            "/hyperlinks/{id}/artifacts/{kind}",
            routing::get(download_latest_artifact),
        )
        .route(
            "/hyperlinks/{id}/artifacts/{kind}/inline",
            routing::get(render_latest_artifact_inline),
        )
        .route(
            "/hyperlinks/{id}/artifacts/pdf_source/preview",
            routing::get(render_pdf_source_preview),
        )
        .route(
            "/hyperlinks/{id}/artifacts/{kind}/delete",
            routing::post(delete_artifact_kind),
        )
        .route(
            "/hyperlinks/{id}/artifacts/{kind}/fetch",
            routing::post(fetch_artifact_kind),
        )
        .route("/hyperlinks/{id_or_ext}", routing::get(show_by_path))
        .route("/hyperlinks/{id_or_ext}", routing::patch(update_by_path))
        .route("/hyperlinks/{id_or_ext}", routing::delete(delete_by_path))
}

pub fn routes() -> Router<Context> {
    links()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HyperlinkResponse {
    id: i32,
    title: String,
    url: String,
    raw_url: String,
    source_type: String,
    clicks_count: i32,
    last_clicked_at: Option<String>,
    processing_state: String,
    created_at: String,
    updated_at: String,
}

#[derive(Clone, Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Clone, Debug, Serialize)]
struct DeleteResponse {
    id: i32,
    deleted: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HyperlinksIndexQueryResponse {
    raw_q: String,
    parsed: crate::server::hyperlink_fetcher::ParsedHyperlinkQuery,
    ignored_tokens: Vec<String>,
    free_text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HyperlinksIndexResponse {
    items: Vec<HyperlinkResponse>,
    query: HyperlinksIndexQueryResponse,
}

#[derive(Clone, Debug, Deserialize)]
struct HyperlinkLookupQuery {
    url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HyperlinkLookupResponse {
    status: String,
    id: Option<i32>,
    canonical_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct HyperlinkTagsForm {
    tags: String,
}

#[derive(Clone, Copy, Debug)]
enum ResponseKind {
    Text,
    Json,
}

struct ParsePathError {
    kind: ResponseKind,
    status: StatusCode,
    message: String,
}

impl ParsePathError {
    fn new(kind: ResponseKind, status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            kind,
            status,
            message: message.into(),
        }
    }

    fn into_response(self) -> Response {
        response_error(self.kind, self.status, self.message)
    }
}

async fn index(
    State(state): State<Context>,
    Query(query): Query<HyperlinkFetchQuery>,
    headers: HeaderMap,
) -> Response {
    index_with_kind(
        &state,
        query,
        ResponseKind::Text,
        Flash::from_headers(&headers),
    )
    .await
}

async fn new(headers: HeaderMap) -> Response {
    views::render_html_page_with_flash("New Hyperlink", render_new(), Flash::from_headers(&headers))
}

async fn index_json(
    State(state): State<Context>,
    Query(query): Query<HyperlinkFetchQuery>,
) -> Response {
    index_with_kind(&state, query, ResponseKind::Json, Flash::default()).await
}

async fn lookup_url(
    State(state): State<Context>,
    Query(query): Query<HyperlinkLookupQuery>,
) -> Response {
    let Some(url) = query.url.as_deref() else {
        return (
            StatusCode::OK,
            Json(HyperlinkLookupResponse {
                status: "invalid_url".to_string(),
                id: None,
                canonical_url: None,
            }),
        )
            .into_response();
    };

    let canonicalized = match crate::model::url_canonicalize::canonicalize_submitted_url(url) {
        Ok(canonicalized) => canonicalized,
        Err(_) => {
            return (
                StatusCode::OK,
                Json(HyperlinkLookupResponse {
                    status: "invalid_url".to_string(),
                    id: None,
                    canonical_url: None,
                }),
            )
                .into_response();
        }
    };

    match crate::model::hyperlink::find_by_url(&state.connection, &canonicalized.canonical_url)
        .await
    {
        Ok(Some(link)) => {
            let status = if link.discovery_depth == crate::model::hyperlink::ROOT_DISCOVERY_DEPTH {
                "root"
            } else {
                "discovered"
            };
            (
                StatusCode::OK,
                Json(HyperlinkLookupResponse {
                    status: status.to_string(),
                    id: Some(link.id),
                    canonical_url: Some(canonicalized.canonical_url),
                }),
            )
                .into_response()
        }
        Ok(None) => (
            StatusCode::OK,
            Json(HyperlinkLookupResponse {
                status: "not_found".to_string(),
                id: None,
                canonical_url: Some(canonicalized.canonical_url),
            }),
        )
            .into_response(),
        Err(err) => response_error(
            ResponseKind::Json,
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to lookup hyperlink by url: {err}"),
        ),
    }
}

async fn create(
    State(state): State<Context>,
    headers: HeaderMap,
    Form(input): Form<HyperlinkInput>,
) -> Response {
    create_with_kind(&state, input, ResponseKind::Text, Some(&headers)).await
}

async fn create_json(State(state): State<Context>, Json(input): Json<HyperlinkInput>) -> Response {
    create_with_kind(&state, input, ResponseKind::Json, None).await
}

async fn show_by_path(
    Path(id_or_ext): Path<String>,
    State(state): State<Context>,
    headers: HeaderMap,
) -> Response {
    let (id, kind) = match parse_id_and_kind(&id_or_ext) {
        Ok(parts) => parts,
        Err(err) => return err.into_response(),
    };
    let flash = match kind {
        ResponseKind::Text => Flash::from_headers(&headers),
        ResponseKind::Json => Flash::default(),
    };
    show_with_kind(&state, id, kind, flash).await
}

async fn update_by_path(
    Path(id_or_ext): Path<String>,
    State(state): State<Context>,
    request: Request,
) -> Response {
    let (id, kind) = match parse_id_and_kind(&id_or_ext) {
        Ok(parts) => parts,
        Err(err) => return err.into_response(),
    };
    let input = match parse_update_input(kind, request).await {
        Ok(input) => input,
        Err(response) => return response,
    };
    update_with_kind(&state, id, input, kind, None).await
}

async fn update_html_post(
    Path(id): Path<i32>,
    State(state): State<Context>,
    headers: HeaderMap,
    Form(input): Form<HyperlinkInput>,
) -> Response {
    update_with_kind(&state, id, input, ResponseKind::Text, Some(&headers)).await
}

async fn delete_by_path(Path(id_or_ext): Path<String>, State(state): State<Context>) -> Response {
    let (id, kind) = match parse_id_and_kind(&id_or_ext) {
        Ok(parts) => parts,
        Err(err) => return err.into_response(),
    };
    if matches!(kind, ResponseKind::Json) {
        return response_error(
            kind,
            StatusCode::NOT_FOUND,
            "delete json endpoint is not supported",
        );
    }
    delete_with_kind(&state, id, kind, None).await
}

async fn delete_html_post(
    Path(id): Path<i32>,
    State(state): State<Context>,
    headers: HeaderMap,
) -> Response {
    delete_with_kind(&state, id, ResponseKind::Text, Some(&headers)).await
}

async fn reprocess(
    Path(id): Path<i32>,
    State(state): State<Context>,
    headers: HeaderMap,
) -> Response {
    match crate::model::hyperlink::enqueue_reprocess_by_id(
        &state.connection,
        id,
        state.processing_queue.as_ref(),
    )
    .await
    {
        Ok(Some((link, true))) => redirect_with_flash(
            &headers,
            &show_path(link.id),
            FlashName::Notice,
            "Queued reprocessing.",
        ),
        Ok(Some((link, false))) => redirect_with_flash(
            &headers,
            &show_path(link.id),
            FlashName::Notice,
            "Reprocessing skipped because source artifact collection is disabled.",
        ),
        Ok(None) => hyperlink_not_found(ResponseKind::Text, id),
        Err(err) => hyperlink_internal_error(ResponseKind::Text, id, "enqueue for processing", err),
    }
}

async fn update_tags_html_post(
    Path(id): Path<i32>,
    State(state): State<Context>,
    headers: HeaderMap,
    Form(form): Form<HyperlinkTagsForm>,
) -> Response {
    let link = match hyperlink::Entity::find_by_id(id)
        .one(&state.connection)
        .await
    {
        Ok(Some(link)) => link,
        Ok(None) => return hyperlink_not_found(ResponseKind::Text, id),
        Err(err) => return hyperlink_internal_error(ResponseKind::Text, id, "fetch", err),
    };

    let tagging_settings = match tagging_settings::load(&state.connection).await {
        Ok(settings) => settings,
        Err(err) => {
            return hyperlink_internal_error(
                ResponseKind::Text,
                id,
                "load tagging settings for",
                err,
            );
        }
    };

    let parsed_tags = hyperlink_tagging::parse_manual_tags_input(&form.tags);
    let normalized_tags = hyperlink_tagging::normalize_manual_tags_for_vocabulary(
        &parsed_tags,
        &tagging_settings.vocabulary,
    );

    if !parsed_tags.is_empty() && normalized_tags.is_empty() {
        return redirect_with_flash(
            &headers,
            &show_path(link.id),
            FlashName::Alert,
            "No valid tags matched the configured vocabulary.",
        );
    }

    if let Err(err) = hyperlink_tagging::persist_manual_tags(
        &state.connection,
        link.id,
        normalized_tags.clone(),
        chrono::Utc::now().to_rfc3339(),
    )
    .await
    {
        return hyperlink_internal_error(ResponseKind::Text, id, "save manual tags for", err);
    }

    let message = if normalized_tags.is_empty() {
        "Saved manual tags (empty override)."
    } else {
        "Saved manual tags."
    };

    redirect_with_flash(&headers, &show_path(link.id), FlashName::Notice, message)
}

async fn delete_artifact_kind(
    Path((id, kind)): Path<(i32, String)>,
    State(state): State<Context>,
    headers: HeaderMap,
) -> Response {
    let Some(kind) = parse_artifact_kind(&kind) else {
        return response_error(
            ResponseKind::Text,
            StatusCode::BAD_REQUEST,
            "invalid artifact kind",
        );
    };

    match hyperlink::Entity::find_by_id(id)
        .one(&state.connection)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return hyperlink_not_found(ResponseKind::Text, id),
        Err(err) => return hyperlink_internal_error(ResponseKind::Text, id, "fetch", err),
    }

    let delete_result = match hyperlink_artifact::Entity::delete_many()
        .filter(hyperlink_artifact::Column::HyperlinkId.eq(id))
        .filter(hyperlink_artifact::Column::Kind.eq(kind.clone()))
        .exec(&state.connection)
        .await
    {
        Ok(result) => result,
        Err(err) => {
            return hyperlink_internal_error(ResponseKind::Text, id, "delete artifacts for", err);
        }
    };

    if is_readability_artifact_kind(&kind)
        && let Err(error) =
            hyperlink_search_doc::clear_readable_text_for_hyperlink(&state.connection, id).await
    {
        if !hyperlink_search_doc::is_search_doc_missing_error(&error) {
            return hyperlink_internal_error(
                ResponseKind::Text,
                id,
                "clear readability search text for",
                error,
            );
        }
    }

    let label = artifact_kind_label(&kind);
    let message = if delete_result.rows_affected > 0 {
        format!("Deleted {label} artifact(s).")
    } else {
        format!("No {label} artifacts to delete.")
    };

    redirect_with_flash(&headers, &show_path(id), FlashName::Notice, message)
}

async fn fetch_artifact_kind(
    Path((id, kind)): Path<(i32, String)>,
    State(state): State<Context>,
    headers: HeaderMap,
) -> Response {
    let Some(kind) = parse_artifact_kind(&kind) else {
        return response_error(
            ResponseKind::Text,
            StatusCode::BAD_REQUEST,
            "invalid artifact kind",
        );
    };

    let hyperlink = match hyperlink::Entity::find_by_id(id)
        .one(&state.connection)
        .await
    {
        Ok(Some(model)) => model,
        Ok(None) => return hyperlink_not_found(ResponseKind::Text, id),
        Err(err) => return hyperlink_internal_error(ResponseKind::Text, id, "fetch", err),
    };

    let Some(job_kind) = artifact_fetch_job_kind(&kind) else {
        return response_error(
            ResponseKind::Text,
            StatusCode::BAD_REQUEST,
            "unsupported artifact fetch kind",
        );
    };

    let Some(queue) = state.processing_queue.as_ref() else {
        return redirect_with_flash(
            &headers,
            &show_path(id),
            FlashName::Alert,
            "Queue workers are unavailable in this environment.",
        );
    };

    if is_screenshot_fetch_artifact_kind(&kind)
        && matches!(hyperlink.source_type, HyperlinkSourceType::Pdf)
    {
        let has_pdf_source = match crate::model::hyperlink_artifact::latest_for_hyperlink_kind(
            &state.connection,
            id,
            HyperlinkArtifactKind::PdfSource,
        )
        .await
        {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(err) => {
                return hyperlink_internal_error(ResponseKind::Text, id, "load artifact for", err);
            }
        };

        if !has_pdf_source {
            return redirect_with_flash(
                &headers,
                &show_path(id),
                FlashName::Alert,
                "Fetch PDF Source first before requesting screenshot or thumbnail artifacts.",
            );
        }
    }

    if let Err(err) = crate::model::hyperlink_processing_job::enqueue_for_hyperlink_kind(
        &state.connection,
        id,
        job_kind,
        Some(queue),
    )
    .await
    {
        return hyperlink_internal_error(ResponseKind::Text, id, "enqueue artifact fetch for", err);
    }

    redirect_with_flash(
        &headers,
        &show_path(id),
        FlashName::Notice,
        format!("Queued fetch for {}.", artifact_kind_label(&kind)),
    )
}

async fn download_latest_artifact(
    Path((id, kind)): Path<(i32, String)>,
    State(state): State<Context>,
) -> Response {
    serve_latest_artifact(state, id, kind, true).await
}

async fn render_latest_artifact_inline(
    Path((id, kind)): Path<(i32, String)>,
    State(state): State<Context>,
) -> Response {
    serve_latest_artifact(state, id, kind, false).await
}

async fn render_pdf_source_preview(Path(id): Path<i32>, State(state): State<Context>) -> Response {
    match hyperlink::Entity::find_by_id(id)
        .one(&state.connection)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return hyperlink_not_found(ResponseKind::Text, id),
        Err(err) => return hyperlink_internal_error(ResponseKind::Text, id, "fetch", err),
    }

    match crate::model::hyperlink_artifact::latest_for_hyperlink_kind(
        &state.connection,
        id,
        HyperlinkArtifactKind::PdfSource,
    )
    .await
    {
        Ok(Some(_)) => {}
        Ok(None) => {
            return response_error(
                ResponseKind::Text,
                StatusCode::NOT_FOUND,
                format!("no pdf_source artifact available for hyperlink {id}"),
            );
        }
        Err(err) => {
            return hyperlink_internal_error(ResponseKind::Text, id, "load artifact for", err);
        }
    }

    let inline_path = artifact_inline_path(id, &HyperlinkArtifactKind::PdfSource);
    let body = format!(
        r#"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <style>
      html, body {{
        margin: 0;
        width: 100%;
        height: 100%;
        background: #fff;
        color-scheme: light;
      }}
      embed {{
        display: block;
        width: 100%;
        height: 100%;
        background: #fff;
      }}
    </style>
  </head>
  <body>
    <embed src="{inline_path}#zoom=page-width" type="application/pdf">
  </body>
</html>
"#,
    );

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        body,
    )
        .into_response()
}

async fn serve_latest_artifact(
    state: Context,
    id: i32,
    kind: String,
    as_attachment: bool,
) -> Response {
    let Some(kind) = parse_artifact_kind(&kind) else {
        return response_error(
            ResponseKind::Text,
            StatusCode::BAD_REQUEST,
            "invalid artifact kind",
        );
    };

    match hyperlink::Entity::find_by_id(id)
        .one(&state.connection)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return hyperlink_not_found(ResponseKind::Text, id),
        Err(err) => return hyperlink_internal_error(ResponseKind::Text, id, "fetch", err),
    }

    let artifact = match crate::model::hyperlink_artifact::latest_for_hyperlink_kind(
        &state.connection,
        id,
        kind.clone(),
    )
    .await
    {
        Ok(Some(artifact)) => artifact,
        Ok(None) => {
            return response_error(
                ResponseKind::Text,
                StatusCode::NOT_FOUND,
                format!(
                    "no {} artifact available for hyperlink {id}",
                    artifact_kind_slug(&kind)
                ),
            );
        }
        Err(err) => {
            return hyperlink_internal_error(ResponseKind::Text, id, "load artifact for", err);
        }
    };

    let payload = match crate::model::hyperlink_artifact::load_payload(&artifact).await {
        Ok(payload) => payload,
        Err(err) => {
            return hyperlink_internal_error(
                ResponseKind::Text,
                id,
                "read artifact payload for",
                err,
            );
        }
    };

    let mut response = Response::new(Body::from(payload));
    *response.status_mut() = StatusCode::OK;

    let content_type = HeaderValue::from_str(&artifact.content_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, content_type);

    if as_attachment {
        let extension = artifact_download_file_extension(&kind, &artifact);
        let filename = format!("hyperlink-{id}-{}.{}", artifact_kind_slug(&kind), extension);
        if let Ok(disposition) =
            HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
        {
            response
                .headers_mut()
                .insert(header::CONTENT_DISPOSITION, disposition);
        }
    }

    response
}

async fn index_with_kind(
    state: &Context,
    query: HyperlinkFetchQuery,
    kind: ResponseKind,
    flash: Flash,
) -> Response {
    let results = match HyperlinkFetcher::new(&state.connection, query)
        .fetch()
        .await
    {
        Ok(results) => results,
        Err(err) => {
            return response_error(
                kind,
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to list hyperlinks: {err}"),
            );
        }
    };
    let hyperlink_ids = results.links.iter().map(|link| link.id).collect::<Vec<_>>();
    let tags_by_hyperlink =
        match hyperlink_tagging::latest_for_hyperlinks(&state.connection, &hyperlink_ids).await {
            Ok(tags) => tags,
            Err(err) => {
                return response_error(
                    kind,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to load hyperlink tags: {err}"),
                );
            }
        };

    match kind {
        ResponseKind::Text => views::render_html_page_with_flash(
            "Hyperlinks",
            render_index(&results, &tags_by_hyperlink),
            flash,
        ),
        ResponseKind::Json => {
            let items = results
                .links
                .iter()
                .map(|link| to_response(link, results.latest_jobs.get(&link.id)))
                .collect::<Vec<_>>();
            let response = HyperlinksIndexResponse {
                items,
                query: HyperlinksIndexQueryResponse {
                    raw_q: results.raw_q,
                    parsed: results.parsed_query,
                    ignored_tokens: results.ignored_tokens,
                    free_text: results.free_text,
                },
            };
            (StatusCode::OK, Json(response)).into_response()
        }
    }
}

async fn create_with_kind(
    state: &Context,
    input: HyperlinkInput,
    kind: ResponseKind,
    request_headers: Option<&HeaderMap>,
) -> Response {
    let input = match crate::model::hyperlink::validate_and_normalize(input).await {
        Ok(input) => input,
        Err(msg) => return response_error(kind, StatusCode::BAD_REQUEST, msg),
    };

    match crate::model::hyperlink::insert(&state.connection, input, state.processing_queue.as_ref())
        .await
    {
        Ok(link) => {
            let latest_job = latest_job_optional(state, link.id).await;
            write_success_response(
                kind,
                StatusCode::CREATED,
                &link,
                latest_job.as_ref(),
                request_headers,
                "Saved link.",
            )
        }
        Err(err) => response_error(
            kind,
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to create hyperlink: {err}"),
        ),
    }
}

async fn show_with_kind(state: &Context, id: i32, kind: ResponseKind, flash: Flash) -> Response {
    match hyperlink::Entity::find_by_id(id)
        .one(&state.connection)
        .await
    {
        Ok(Some(link)) => {
            let latest_job = match crate::model::hyperlink_processing_job::latest_for_hyperlink(
                &state.connection,
                id,
            )
            .await
            {
                Ok(job) => job,
                Err(err) => {
                    return hyperlink_internal_error(kind, id, "load processing job for", err);
                }
            };

            let latest_artifacts =
                match crate::model::hyperlink_artifact::latest_for_hyperlink_kinds(
                    &state.connection,
                    id,
                    &show_artifact_kinds(),
                )
                .await
                {
                    Ok(artifacts) => artifacts,
                    Err(err) => {
                        return hyperlink_internal_error(kind, id, "load artifacts for", err);
                    }
                };

            let recent_jobs = match crate::model::hyperlink_processing_job::recent_for_hyperlink(
                &state.connection,
                id,
                SHOW_TIMELINE_LIMIT,
            )
            .await
            {
                Ok(jobs) => jobs,
                Err(err) => {
                    return hyperlink_internal_error(kind, id, "load job history for", err);
                }
            };

            let discovered_links =
                match crate::model::hyperlink_relation::children_for_parent(&state.connection, id)
                    .await
                {
                    Ok(children) => children,
                    Err(err) => {
                        return hyperlink_internal_error(
                            kind,
                            id,
                            "load discovered links for",
                            err,
                        );
                    }
                };
            let discovered_link_ids: Vec<i32> =
                discovered_links.iter().map(|link| link.id).collect();
            let discovered_latest_jobs =
                match crate::model::hyperlink_processing_job::latest_for_hyperlinks(
                    &state.connection,
                    &discovered_link_ids,
                )
                .await
                {
                    Ok(jobs) => jobs,
                    Err(err) => {
                        return hyperlink_internal_error(
                            kind,
                            id,
                            "load discovered link processing jobs for",
                            err,
                        );
                    }
                };
            let discovered_thumbnail_artifacts =
                match crate::model::hyperlink_artifact::latest_for_hyperlinks_kind(
                    &state.connection,
                    &discovered_link_ids,
                    HyperlinkArtifactKind::ScreenshotThumbWebp,
                )
                .await
                {
                    Ok(artifacts) => artifacts,
                    Err(err) => {
                        return hyperlink_internal_error(
                            kind,
                            id,
                            "load discovered link thumbnails for",
                            err,
                        );
                    }
                };
            let discovered_dark_thumbnail_artifacts =
                match crate::model::hyperlink_artifact::latest_for_hyperlinks_kind(
                    &state.connection,
                    &discovered_link_ids,
                    HyperlinkArtifactKind::ScreenshotThumbDarkWebp,
                )
                .await
                {
                    Ok(artifacts) => artifacts,
                    Err(err) => {
                        return hyperlink_internal_error(
                            kind,
                            id,
                            "load discovered link dark thumbnails for",
                            err,
                        );
                    }
                };
            let link_tag_set =
                match hyperlink_tagging::latest_for_hyperlink(&state.connection, id).await {
                    Ok(tags) => tags,
                    Err(err) => {
                        return hyperlink_internal_error(kind, id, "load tags for", err);
                    }
                };
            let discovered_tags_by_hyperlink = match hyperlink_tagging::latest_for_hyperlinks(
                &state.connection,
                &discovered_link_ids,
            )
            .await
            {
                Ok(tags) => tags,
                Err(err) => {
                    return hyperlink_internal_error(kind, id, "load discovered tags for", err);
                }
            };
            let tagging_settings = match tagging_settings::load(&state.connection).await {
                Ok(settings) => settings,
                Err(err) => {
                    return hyperlink_internal_error(kind, id, "load tagging settings for", err);
                }
            };

            match kind {
                ResponseKind::Text => {
                    let og_summary = load_og_summary(&link);
                    let display_title = select_show_display_title(&link, og_summary.as_ref());
                    views::render_html_page_with_flash(
                        "Show Hyperlink",
                        render_show(
                            &link,
                            latest_job.as_ref(),
                            &latest_artifacts,
                            &recent_jobs,
                            &discovered_links,
                            &discovered_latest_jobs,
                            &discovered_thumbnail_artifacts,
                            &discovered_dark_thumbnail_artifacts,
                            link_tag_set.as_ref(),
                            &discovered_tags_by_hyperlink,
                            &tagging_settings.vocabulary,
                            display_title,
                            og_summary,
                        ),
                        flash,
                    )
                }
                ResponseKind::Json => (
                    StatusCode::OK,
                    Json(to_response(&link, latest_job.as_ref())),
                )
                    .into_response(),
            }
        }
        Ok(None) => hyperlink_not_found(kind, id),
        Err(err) => hyperlink_internal_error(kind, id, "fetch", err),
    }
}

async fn visit(Path(id): Path<i32>, State(state): State<Context>) -> Response {
    match crate::model::hyperlink::increment_click_count_by_id(&state.connection, id).await {
        Ok(Some(link)) => Redirect::temporary(&link.url).into_response(),
        Ok(None) => hyperlink_not_found(ResponseKind::Text, id),
        Err(err) => hyperlink_internal_error(ResponseKind::Text, id, "visit", err),
    }
}

async fn click(Path(id): Path<i32>, State(state): State<Context>) -> Response {
    match crate::model::hyperlink::increment_click_count_by_id(&state.connection, id).await {
        Ok(Some(_)) => StatusCode::NO_CONTENT.into_response(),
        Ok(None) => hyperlink_not_found(ResponseKind::Text, id),
        Err(err) => hyperlink_internal_error(ResponseKind::Text, id, "track click for", err),
    }
}

async fn edit(Path(id): Path<i32>, State(state): State<Context>, headers: HeaderMap) -> Response {
    match hyperlink::Entity::find_by_id(id)
        .one(&state.connection)
        .await
    {
        Ok(Some(link)) => views::render_html_page_with_flash(
            "Edit Hyperlink",
            render_edit(&link),
            Flash::from_headers(&headers),
        ),
        Ok(None) => hyperlink_not_found(ResponseKind::Text, id),
        Err(err) => hyperlink_internal_error(ResponseKind::Text, id, "fetch", err),
    }
}

async fn update_with_kind(
    state: &Context,
    id: i32,
    input: HyperlinkInput,
    kind: ResponseKind,
    request_headers: Option<&HeaderMap>,
) -> Response {
    let input = match crate::model::hyperlink::validate_and_normalize(input).await {
        Ok(input) => input,
        Err(msg) => return response_error(kind, StatusCode::BAD_REQUEST, msg),
    };

    match crate::model::hyperlink::update_by_id(
        &state.connection,
        id,
        input,
        state.processing_queue.as_ref(),
    )
    .await
    {
        Ok(Some(link)) => {
            let latest_job = latest_job_optional(state, link.id).await;
            write_success_response(
                kind,
                StatusCode::OK,
                &link,
                latest_job.as_ref(),
                request_headers,
                "Updated link.",
            )
        }
        Ok(None) => hyperlink_not_found(kind, id),
        Err(err) => hyperlink_internal_error(kind, id, "update", err),
    }
}

async fn delete_with_kind(
    state: &Context,
    id: i32,
    kind: ResponseKind,
    request_headers: Option<&HeaderMap>,
) -> Response {
    match crate::model::hyperlink::delete_by_id_with_tombstone(&state.connection, id).await {
        Ok(false) => hyperlink_not_found(kind, id),
        Ok(_) => match kind {
            ResponseKind::Text => {
                if let Some(headers) = request_headers {
                    return redirect_with_flash(
                        headers,
                        HYPERLINKS_PATH,
                        FlashName::Notice,
                        "Deleted link.",
                    );
                }
                Redirect::to(HYPERLINKS_PATH).into_response()
            }
            ResponseKind::Json => {
                (StatusCode::OK, Json(DeleteResponse { id, deleted: true })).into_response()
            }
        },
        Err(err) => hyperlink_internal_error(kind, id, "delete", err),
    }
}

fn parse_id_and_kind(id_or_ext: &str) -> Result<(i32, ResponseKind), ParsePathError> {
    let (raw_id, kind) = if let Some(raw_id) = id_or_ext.strip_suffix(".json") {
        (raw_id, ResponseKind::Json)
    } else {
        (id_or_ext, ResponseKind::Text)
    };

    if raw_id.is_empty() {
        return Err(ParsePathError::new(
            kind,
            StatusCode::BAD_REQUEST,
            "invalid hyperlink id",
        ));
    }

    match raw_id.parse::<i32>() {
        Ok(id) => Ok((id, kind)),
        Err(_) => Err(ParsePathError::new(
            kind,
            StatusCode::BAD_REQUEST,
            format!("invalid hyperlink id: {raw_id}"),
        )),
    }
}

async fn parse_update_input(
    kind: ResponseKind,
    request: Request,
) -> Result<HyperlinkInput, Response> {
    let body = match body::to_bytes(request.into_body(), usize::MAX).await {
        Ok(body) => body,
        Err(err) => {
            return Err(response_error(
                kind,
                StatusCode::BAD_REQUEST,
                format!("failed to read request body: {err}"),
            ));
        }
    };

    let parsed = match kind {
        ResponseKind::Text => serde_urlencoded::from_bytes::<HyperlinkInput>(&body)
            .map_err(|err| format!("invalid form payload: {err}")),
        ResponseKind::Json => serde_json::from_slice::<HyperlinkInput>(&body)
            .map_err(|err| format!("invalid json payload: {err}")),
    };

    parsed.map_err(|message| response_error(kind, StatusCode::BAD_REQUEST, message))
}

fn to_response(
    model: &hyperlink::Model,
    latest_job: Option<&hyperlink_processing_job::Model>,
) -> HyperlinkResponse {
    HyperlinkResponse {
        id: model.id,
        title: normalize_link_title_for_display(
            model.title.as_str(),
            model.url.as_str(),
            model.raw_url.as_str(),
        ),
        url: model.url.clone(),
        raw_url: model.raw_url.clone(),
        source_type: hyperlink_source_type_name(&model.source_type).to_string(),
        clicks_count: model.clicks_count,
        last_clicked_at: model.last_clicked_at.as_ref().map(ToString::to_string),
        processing_state: processing_state_name(latest_job).to_string(),
        created_at: model.created_at.to_string(),
        updated_at: model.updated_at.to_string(),
    }
}

fn processing_state_name(job: Option<&hyperlink_processing_job::Model>) -> &'static str {
    match job {
        Some(job) => crate::model::hyperlink_processing_job::state_name(job.state.clone()),
        None => "idle",
    }
}

fn hyperlink_source_type_name(source_type: &HyperlinkSourceType) -> &'static str {
    match source_type {
        HyperlinkSourceType::Unknown => "unknown",
        HyperlinkSourceType::Html => "html",
        HyperlinkSourceType::Pdf => "pdf",
    }
}

enum IndexStatus {
    Processing,
    Failed,
}

fn index_status(
    job: Option<&hyperlink_processing_job::Model>,
    active_processing_job_ids: Option<&HashSet<i32>>,
) -> Option<IndexStatus> {
    let job = job?;
    match job.state {
        hyperlink_processing_job::HyperlinkProcessingJobState::Queued
        | hyperlink_processing_job::HyperlinkProcessingJobState::Running => {
            if let Some(active_ids) = active_processing_job_ids {
                active_ids
                    .contains(&job.id)
                    .then_some(IndexStatus::Processing)
            } else {
                Some(IndexStatus::Processing)
            }
        }
        hyperlink_processing_job::HyperlinkProcessingJobState::Failed => Some(IndexStatus::Failed),
        _ => None,
    }
}

fn show_path(id: i32) -> String {
    format!("/hyperlinks/{id}")
}

async fn latest_job_optional(
    state: &Context,
    hyperlink_id: i32,
) -> Option<hyperlink_processing_job::Model> {
    crate::model::hyperlink_processing_job::latest_for_hyperlink(&state.connection, hyperlink_id)
        .await
        .ok()
        .flatten()
}

fn write_success_response(
    kind: ResponseKind,
    status: StatusCode,
    link: &hyperlink::Model,
    latest_job: Option<&hyperlink_processing_job::Model>,
    request_headers: Option<&HeaderMap>,
    notice_message: &str,
) -> Response {
    match kind {
        ResponseKind::Text => {
            if let Some(headers) = request_headers {
                return redirect_with_flash(
                    headers,
                    &show_path(link.id),
                    FlashName::Notice,
                    notice_message,
                );
            }
            Redirect::to(&show_path(link.id)).into_response()
        }
        ResponseKind::Json => (status, Json(to_response(link, latest_job))).into_response(),
    }
}

fn hyperlink_not_found(kind: ResponseKind, id: i32) -> Response {
    response_error(
        kind,
        StatusCode::NOT_FOUND,
        format!("hyperlink {id} not found"),
    )
}

fn hyperlink_internal_error(
    kind: ResponseKind,
    id: i32,
    action: &str,
    err: impl Display,
) -> Response {
    response_error(
        kind,
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("failed to {action} hyperlink {id}: {err}"),
    )
}

#[derive(Template)]
#[template(path = "hyperlinks/index.stpl")]
struct HyperlinksIndexTemplate<'a> {
    links: &'a [hyperlink::Model],
    latest_jobs: &'a HashMap<i32, hyperlink_processing_job::Model>,
    active_processing_job_ids: &'a HashSet<i32>,
    thumbnail_artifacts: &'a HashMap<i32, hyperlink_artifact::Model>,
    dark_thumbnail_artifacts: &'a HashMap<i32, hyperlink_artifact::Model>,
    tags_by_hyperlink: &'a HashMap<i32, hyperlink_tagging::TagSet>,
    match_snippets: &'a HashMap<i32, String>,
    parsed_query: &'a crate::server::hyperlink_fetcher::ParsedHyperlinkQuery,
    query_input: &'a str,
    ignored_tokens: &'a [String],
    free_text: &'a str,
    page: u64,
    total_pages: u64,
    prev_page_href: Option<String>,
    next_page_href: Option<String>,
}

impl<'a> HyperlinksIndexTemplate<'a> {
    fn index_status(&self, job: Option<&hyperlink_processing_job::Model>) -> Option<IndexStatus> {
        index_status(job, Some(self.active_processing_job_ids))
    }

    fn has_free_text(&self) -> bool {
        !self.free_text.trim().is_empty()
    }

    fn status_select_value(&self) -> &'static str {
        if self.parsed_query.statuses.is_empty()
            || self.parsed_query.statuses.contains(&StatusToken::All)
        {
            return "all";
        }

        if self.parsed_query.statuses.len() != 1 {
            return "all";
        }

        match self.parsed_query.statuses[0] {
            StatusToken::All => "all",
            StatusToken::Processing => "processing",
            StatusToken::Failed => "failed",
            StatusToken::Idle => "idle",
            StatusToken::Succeeded => "succeeded",
        }
    }

    fn type_select_value(&self) -> &'static str {
        let has_all = self.parsed_query.types.contains(&TypeToken::All);
        let has_pdf = self.parsed_query.types.contains(&TypeToken::Pdf);
        let has_non_pdf = self.parsed_query.types.contains(&TypeToken::NonPdf);

        if has_all || (has_pdf && has_non_pdf) || self.parsed_query.types.is_empty() {
            "all"
        } else if has_pdf {
            "pdf"
        } else {
            "non-pdf"
        }
    }

    fn order_select_value(&self) -> &'static str {
        let has_free_text = self.has_free_text();
        let effective_order = match self.parsed_query.orders.last().copied() {
            Some(OrderToken::Relevance) if !has_free_text => OrderToken::Newest,
            Some(order) => order,
            None if has_free_text => OrderToken::Relevance,
            None => OrderToken::Newest,
        };

        match effective_order {
            OrderToken::Newest => "newest",
            OrderToken::Oldest => "oldest",
            OrderToken::MostClicked => "most-clicked",
            OrderToken::RecentlyClicked => "recently-clicked",
            OrderToken::Random => "random",
            OrderToken::Relevance => "relevance",
        }
    }

    fn show_discovered_links_checked(&self) -> bool {
        let has_all = self.parsed_query.scopes.contains(&ScopeToken::All);
        let has_discovered = self.parsed_query.scopes.contains(&ScopeToken::Discovered);

        has_all || has_discovered
    }

    fn mobile_filters_open(&self) -> bool {
        !self.parsed_query.statuses.is_empty()
            || !self.parsed_query.types.is_empty()
            || !self.parsed_query.orders.is_empty()
            || self.show_discovered_links_checked()
    }

    fn has_effective_filter(&self) -> bool {
        let status_filters_active = !self.parsed_query.statuses.is_empty()
            && !self.parsed_query.statuses.contains(&StatusToken::All);

        let has_scope_all = self.parsed_query.scopes.contains(&ScopeToken::All);
        let has_scope_root = self.parsed_query.scopes.contains(&ScopeToken::Root);
        let has_scope_discovered = self.parsed_query.scopes.contains(&ScopeToken::Discovered);
        let scope_filter_active = has_scope_discovered && !has_scope_all && !has_scope_root;

        let has_type_all = self.parsed_query.types.contains(&TypeToken::All);
        let has_type_pdf = self.parsed_query.types.contains(&TypeToken::Pdf);
        let has_type_non_pdf = self.parsed_query.types.contains(&TypeToken::NonPdf);
        let type_filter_active = !has_type_all && (has_type_pdf ^ has_type_non_pdf);

        status_filters_active || scope_filter_active || type_filter_active || self.has_free_text()
    }

    fn link_display_title(&self, link: &hyperlink::Model) -> String {
        normalize_link_title_for_display(
            link.title.as_str(),
            link.url.as_str(),
            link.raw_url.as_str(),
        )
    }

    fn thumbnail_inline_path(&self, hyperlink_id: i32) -> Option<String> {
        self.thumbnail_artifacts
            .contains_key(&hyperlink_id)
            .then_some(artifact_inline_path(
                hyperlink_id,
                &HyperlinkArtifactKind::ScreenshotThumbWebp,
            ))
    }

    fn thumbnail_dark_inline_path(&self, hyperlink_id: i32) -> Option<String> {
        self.dark_thumbnail_artifacts
            .contains_key(&hyperlink_id)
            .then_some(artifact_inline_path(
                hyperlink_id,
                &HyperlinkArtifactKind::ScreenshotThumbDarkWebp,
            ))
    }

    fn link_is_pdf(&self, hyperlink: &hyperlink::Model) -> bool {
        matches!(hyperlink.source_type, HyperlinkSourceType::Pdf)
    }

    fn link_primary_tag(&self, hyperlink_id: i32) -> Option<&str> {
        self.tags_by_hyperlink
            .get(&hyperlink_id)
            .and_then(|meta| meta.primary_visible_tag.as_deref())
    }
}

fn render_index(
    results: &HyperlinkFetchResults,
    tags_by_hyperlink: &HashMap<i32, hyperlink_tagging::TagSet>,
) -> Result<String, sailfish::RenderError> {
    let prev_page_href = if results.page > 1 {
        Some(hyperlinks_index_href(&results.raw_q, results.page - 1))
    } else {
        None
    };
    let next_page_href = if results.page < results.total_pages {
        Some(hyperlinks_index_href(&results.raw_q, results.page + 1))
    } else {
        None
    };

    HyperlinksIndexTemplate {
        links: &results.links,
        latest_jobs: &results.latest_jobs,
        active_processing_job_ids: &results.active_processing_job_ids,
        thumbnail_artifacts: &results.thumbnail_artifacts,
        dark_thumbnail_artifacts: &results.dark_thumbnail_artifacts,
        tags_by_hyperlink,
        match_snippets: &results.match_snippets,
        parsed_query: &results.parsed_query,
        query_input: &results.raw_q,
        ignored_tokens: &results.ignored_tokens,
        free_text: &results.free_text,
        page: results.page,
        total_pages: results.total_pages,
        prev_page_href,
        next_page_href,
    }
    .render()
}

#[derive(Serialize)]
struct HyperlinksIndexHrefQuery<'a> {
    #[serde(skip_serializing_if = "str::is_empty")]
    q: &'a str,
    page: u64,
}

fn hyperlinks_index_href(raw_q: &str, page: u64) -> String {
    let query = serde_urlencoded::to_string(HyperlinksIndexHrefQuery { q: raw_q, page })
        .unwrap_or_else(|_| format!("page={page}"));
    format!("{HYPERLINKS_PATH}?{query}")
}

#[derive(Template)]
#[template(path = "hyperlinks/new.stpl")]
struct HyperlinksNewTemplate;

fn render_new() -> Result<String, sailfish::RenderError> {
    HyperlinksNewTemplate.render()
}

#[derive(Template)]
#[template(path = "hyperlinks/show.stpl")]
struct HyperlinksShowTemplate<'a> {
    link: &'a hyperlink::Model,
    latest_job: Option<&'a hyperlink_processing_job::Model>,
    latest_artifacts: &'a HashMap<HyperlinkArtifactKind, hyperlink_artifact::Model>,
    recent_jobs: &'a [hyperlink_processing_job::Model],
    discovered_links: &'a [hyperlink::Model],
    discovered_latest_jobs: &'a HashMap<i32, hyperlink_processing_job::Model>,
    discovered_thumbnail_artifacts: &'a HashMap<i32, hyperlink_artifact::Model>,
    discovered_dark_thumbnail_artifacts: &'a HashMap<i32, hyperlink_artifact::Model>,
    tag_set: Option<&'a hyperlink_tagging::TagSet>,
    discovered_tags_by_hyperlink: &'a HashMap<i32, hyperlink_tagging::TagSet>,
    tagging_vocabulary: &'a [String],
    display_title: String,
    og_summary: Option<OgSummary>,
}

impl<'a> HyperlinksShowTemplate<'a> {
    fn index_status(&self, job: Option<&hyperlink_processing_job::Model>) -> Option<IndexStatus> {
        index_status(job, None)
    }

    fn display_title(&self) -> &str {
        self.display_title.as_str()
    }

    fn link_display_title(&self, link: &hyperlink::Model) -> String {
        normalize_link_title_for_display(
            link.title.as_str(),
            link.url.as_str(),
            link.raw_url.as_str(),
        )
    }

    fn processing_state(&self) -> &'static str {
        processing_state_name(self.latest_job)
    }

    fn latest_error_message(&self) -> Option<&str> {
        self.latest_job
            .filter(|job| {
                matches!(
                    job.state,
                    hyperlink_processing_job::HyperlinkProcessingJobState::Failed
                )
            })
            .and_then(|job| job.error_message.as_deref())
    }

    fn present_artifact_kinds(&self) -> Vec<HyperlinkArtifactKind> {
        show_artifact_kinds()
            .into_iter()
            .filter(|kind| self.latest_artifacts.contains_key(kind))
            .collect()
    }

    fn missing_artifact_kinds(&self) -> Vec<HyperlinkArtifactKind> {
        required_show_artifact_kinds(&self.link.source_type, self.latest_artifacts)
            .into_iter()
            .filter(|kind| !self.latest_artifacts.contains_key(kind))
            .collect()
    }

    fn artifact_for(&self, kind: &HyperlinkArtifactKind) -> Option<&hyperlink_artifact::Model> {
        self.latest_artifacts.get(kind)
    }

    fn artifact_download_path(&self, kind: &HyperlinkArtifactKind) -> String {
        artifact_download_path(self.link.id, kind)
    }

    fn artifact_inline_path(&self, kind: &HyperlinkArtifactKind) -> String {
        artifact_inline_path(self.link.id, kind)
    }

    fn artifact_delete_path(&self, kind: &HyperlinkArtifactKind) -> String {
        artifact_delete_path(self.link.id, kind)
    }

    fn artifact_fetch_path(&self, kind: &HyperlinkArtifactKind) -> String {
        artifact_fetch_path(self.link.id, kind)
    }

    fn og_summary(&self) -> Option<&OgSummary> {
        self.og_summary.as_ref()
    }

    fn og_meta_inline_path(&self) -> Option<String> {
        self.latest_artifacts
            .contains_key(&HyperlinkArtifactKind::OgMeta)
            .then_some(self.artifact_inline_path(&HyperlinkArtifactKind::OgMeta))
    }

    fn pdf_preview_path(&self) -> Option<String> {
        self.latest_artifacts
            .contains_key(&HyperlinkArtifactKind::PdfSource)
            .then_some(artifact_pdf_preview_path(self.link.id))
    }

    fn screenshot_inline_path(&self) -> Option<String> {
        self.latest_artifacts
            .contains_key(&HyperlinkArtifactKind::ScreenshotWebp)
            .then_some(self.artifact_inline_path(&HyperlinkArtifactKind::ScreenshotWebp))
    }

    fn screenshot_dark_inline_path(&self) -> Option<String> {
        self.latest_artifacts
            .contains_key(&HyperlinkArtifactKind::ScreenshotDarkWebp)
            .then_some(self.artifact_inline_path(&HyperlinkArtifactKind::ScreenshotDarkWebp))
    }

    fn thumbnail_inline_path(&self, hyperlink_id: i32) -> Option<String> {
        if hyperlink_id == self.link.id
            && self
                .latest_artifacts
                .contains_key(&HyperlinkArtifactKind::ScreenshotThumbWebp)
        {
            return Some(self.artifact_inline_path(&HyperlinkArtifactKind::ScreenshotThumbWebp));
        }

        self.discovered_thumbnail_artifacts
            .contains_key(&hyperlink_id)
            .then_some(artifact_inline_path(
                hyperlink_id,
                &HyperlinkArtifactKind::ScreenshotThumbWebp,
            ))
    }

    fn thumbnail_dark_inline_path(&self, hyperlink_id: i32) -> Option<String> {
        if hyperlink_id == self.link.id
            && self
                .latest_artifacts
                .contains_key(&HyperlinkArtifactKind::ScreenshotThumbDarkWebp)
        {
            return Some(
                self.artifact_inline_path(&HyperlinkArtifactKind::ScreenshotThumbDarkWebp),
            );
        }

        self.discovered_dark_thumbnail_artifacts
            .contains_key(&hyperlink_id)
            .then_some(artifact_inline_path(
                hyperlink_id,
                &HyperlinkArtifactKind::ScreenshotThumbDarkWebp,
            ))
    }

    fn link_is_pdf(&self, hyperlink: &hyperlink::Model) -> bool {
        matches!(hyperlink.source_type, HyperlinkSourceType::Pdf)
    }

    fn link_primary_tag(&self, hyperlink_id: i32) -> Option<&str> {
        if hyperlink_id == self.link.id {
            return self
                .tag_set
                .and_then(|set| set.primary_visible_tag.as_deref());
        }

        self.discovered_tags_by_hyperlink
            .get(&hyperlink_id)
            .and_then(|set| set.primary_visible_tag.as_deref())
    }

    fn visible_tags(&self) -> &[String] {
        self.tag_set
            .map(|set| set.visible_tags.as_slice())
            .unwrap_or(&[])
    }

    fn manual_tags_input_value(&self) -> String {
        let user_tags = self
            .tag_set
            .map(|set| set.user_tags.as_slice())
            .unwrap_or(&[]);
        if user_tags.is_empty() {
            return self.visible_tags().join(", ");
        }
        user_tags.join(", ")
    }

    fn tagging_vocabulary_hint(&self) -> String {
        self.tagging_vocabulary.join(", ")
    }
}

fn render_show(
    link: &hyperlink::Model,
    latest_job: Option<&hyperlink_processing_job::Model>,
    latest_artifacts: &HashMap<HyperlinkArtifactKind, hyperlink_artifact::Model>,
    recent_jobs: &[hyperlink_processing_job::Model],
    discovered_links: &[hyperlink::Model],
    discovered_latest_jobs: &HashMap<i32, hyperlink_processing_job::Model>,
    discovered_thumbnail_artifacts: &HashMap<i32, hyperlink_artifact::Model>,
    discovered_dark_thumbnail_artifacts: &HashMap<i32, hyperlink_artifact::Model>,
    tag_set: Option<&hyperlink_tagging::TagSet>,
    discovered_tags_by_hyperlink: &HashMap<i32, hyperlink_tagging::TagSet>,
    tagging_vocabulary: &[String],
    display_title: String,
    og_summary: Option<OgSummary>,
) -> Result<String, sailfish::RenderError> {
    HyperlinksShowTemplate {
        link,
        latest_job,
        latest_artifacts,
        recent_jobs,
        discovered_links,
        discovered_latest_jobs,
        discovered_thumbnail_artifacts,
        discovered_dark_thumbnail_artifacts,
        tag_set,
        discovered_tags_by_hyperlink,
        tagging_vocabulary,
        display_title,
        og_summary,
    }
    .render()
}

#[derive(Template)]
#[template(path = "hyperlinks/edit.stpl")]
struct HyperlinksEditTemplate<'a> {
    link: &'a hyperlink::Model,
}

fn render_edit(link: &hyperlink::Model) -> Result<String, sailfish::RenderError> {
    HyperlinksEditTemplate { link }.render()
}

#[derive(Clone, Debug)]
struct OgSummary {
    title: Option<String>,
    description: Option<String>,
    og_type: Option<String>,
    url: Option<String>,
    image_url: Option<String>,
    site_name: Option<String>,
}

impl OgSummary {
    fn has_values(&self) -> bool {
        self.title.is_some()
            || self.description.is_some()
            || self.og_type.is_some()
            || self.url.is_some()
            || self.image_url.is_some()
            || self.site_name.is_some()
    }

    fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    fn og_type(&self) -> Option<&str> {
        self.og_type.as_deref()
    }

    fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    fn image_url(&self) -> Option<&str> {
        self.image_url.as_deref()
    }

    fn site_name(&self) -> Option<&str> {
        self.site_name.as_deref()
    }
}

fn load_og_summary(link: &hyperlink::Model) -> Option<OgSummary> {
    let summary = OgSummary {
        title: normalize_text_value(link.og_title.as_deref()),
        description: normalize_text_value(link.og_description.as_deref()),
        og_type: normalize_text_value(link.og_type.as_deref()),
        url: normalize_url_value(link.og_url.as_deref()),
        image_url: normalize_url_value(link.og_image_url.as_deref()),
        site_name: normalize_text_value(link.og_site_name.as_deref()),
    };

    summary.has_values().then_some(summary)
}

fn normalize_text_value(value: Option<&str>) -> Option<String> {
    let value = value?;
    let normalized = normalize_display_text(value);
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn normalize_url_value(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_display_text(value: &str) -> String {
    let decoded = decode_html_entities(value.trim());
    collapse_whitespace(decoded.trim())
}

fn normalize_link_title_for_display(title: &str, url: &str, raw_url: &str) -> String {
    let normalized = normalize_display_text(title);
    if normalized.is_empty() {
        return title.to_string();
    }

    let cleaned = hyperlink_title::strip_site_affixes(normalized.as_str(), url, raw_url);
    if cleaned.is_empty() {
        normalized
    } else {
        cleaned
    }
}

fn decode_html_entities(value: &str) -> String {
    if !value.contains('&') {
        return value.to_string();
    }

    let mut decoded = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(entity_start_offset) = value[cursor..].find('&') {
        let amp_index = cursor + entity_start_offset;
        decoded.push_str(&value[cursor..amp_index]);

        let entity_start = amp_index + 1;
        let rest = &value[entity_start..];
        let Some(entity_end_offset) = rest.find(';') else {
            decoded.push('&');
            cursor = entity_start;
            continue;
        };

        let entity_end = entity_start + entity_end_offset;
        let entity = &value[entity_start..entity_end];
        if let Some(decoded_entity) = decode_html_entity(entity) {
            decoded.push(decoded_entity);
            cursor = entity_end + 1;
            continue;
        }

        decoded.push('&');
        cursor = entity_start;
    }

    decoded.push_str(&value[cursor..]);
    decoded
}

fn decode_html_entity(entity: &str) -> Option<char> {
    if let Some(decoded_numeric) = decode_numeric_html_entity(entity) {
        return Some(decoded_numeric);
    }

    match entity.to_ascii_lowercase().as_str() {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{00A0}'),
        _ => None,
    }
}

fn decode_numeric_html_entity(entity: &str) -> Option<char> {
    let value = if let Some(hex) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        entity.strip_prefix('#')?.parse::<u32>().ok()?
    };

    char::from_u32(value)
}

fn select_show_display_title(link: &hyperlink::Model, og_summary: Option<&OgSummary>) -> String {
    if let Some(candidate_title) = og_summary.and_then(|summary| summary.title()) {
        if metadata_title_candidate_is_usable(candidate_title)
            && should_prefer_metadata_title(
                link.title.as_str(),
                link.url.as_str(),
                link.raw_url.as_str(),
                candidate_title,
            )
        {
            return normalize_link_title_for_display(
                candidate_title,
                link.url.as_str(),
                link.raw_url.as_str(),
            );
        }
    }

    normalize_link_title_for_display(
        link.title.as_str(),
        link.url.as_str(),
        link.raw_url.as_str(),
    )
}

fn metadata_title_candidate_is_usable(candidate: &str) -> bool {
    let normalized = normalize_display_text(candidate);
    !normalized.is_empty() && normalized.chars().count() <= 200
}

fn should_prefer_metadata_title(
    current: &str,
    link_url: &str,
    raw_url: &str,
    candidate: &str,
) -> bool {
    let current_title = normalize_display_text(current);
    let candidate_title = normalize_display_text(candidate);
    let current_url_like = looks_like_url_title(&current_title, link_url, raw_url);
    let candidate_url_like = looks_like_url_title(&candidate_title, link_url, raw_url);

    if current_url_like && !candidate_url_like {
        return true;
    }

    let current_len = current_title.chars().count();
    let candidate_len = candidate_title.chars().count();
    if current_len < 12 && candidate_len >= 20 && !candidate_url_like {
        return true;
    }

    word_count(&current_title) == 1 && word_count(&candidate_title) >= 2 && !candidate_url_like
}

fn looks_like_url_title(value: &str, link_url: &str, raw_url: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return true;
    }

    if trimmed.eq_ignore_ascii_case(link_url.trim()) || trimmed.eq_ignore_ascii_case(raw_url.trim())
    {
        return true;
    }

    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn word_count(value: &str) -> usize {
    value.split_whitespace().count()
}

fn show_artifact_kinds() -> [HyperlinkArtifactKind; 15] {
    [
        HyperlinkArtifactKind::SnapshotWarc,
        HyperlinkArtifactKind::PdfSource,
        HyperlinkArtifactKind::PaperlessMetadata,
        HyperlinkArtifactKind::SnapshotError,
        HyperlinkArtifactKind::OgMeta,
        HyperlinkArtifactKind::OgImage,
        HyperlinkArtifactKind::OgError,
        HyperlinkArtifactKind::ScreenshotWebp,
        HyperlinkArtifactKind::ScreenshotThumbWebp,
        HyperlinkArtifactKind::ScreenshotDarkWebp,
        HyperlinkArtifactKind::ScreenshotThumbDarkWebp,
        HyperlinkArtifactKind::ScreenshotError,
        HyperlinkArtifactKind::ReadableText,
        HyperlinkArtifactKind::ReadableMeta,
        HyperlinkArtifactKind::ReadableError,
    ]
}

fn required_show_artifact_kinds(
    source_type: &HyperlinkSourceType,
    latest_artifacts: &HashMap<HyperlinkArtifactKind, hyperlink_artifact::Model>,
) -> Vec<HyperlinkArtifactKind> {
    let mut required = vec![
        required_source_artifact_kind(source_type, latest_artifacts),
        HyperlinkArtifactKind::OgMeta,
        HyperlinkArtifactKind::ReadableText,
        HyperlinkArtifactKind::ReadableMeta,
    ];

    required.extend(required_screenshot_artifact_kinds(
        source_type,
        latest_artifacts,
    ));

    required
}

fn required_source_artifact_kind(
    source_type: &HyperlinkSourceType,
    latest_artifacts: &HashMap<HyperlinkArtifactKind, hyperlink_artifact::Model>,
) -> HyperlinkArtifactKind {
    match source_type {
        HyperlinkSourceType::Pdf => HyperlinkArtifactKind::PdfSource,
        HyperlinkSourceType::Html => HyperlinkArtifactKind::SnapshotWarc,
        HyperlinkSourceType::Unknown => latest_source_artifact_kind(latest_artifacts)
            .unwrap_or(HyperlinkArtifactKind::SnapshotWarc),
    }
}

fn required_screenshot_artifact_kinds(
    source_type: &HyperlinkSourceType,
    latest_artifacts: &HashMap<HyperlinkArtifactKind, hyperlink_artifact::Model>,
) -> Vec<HyperlinkArtifactKind> {
    if matches!(source_type, HyperlinkSourceType::Pdf) {
        if latest_artifacts.contains_key(&HyperlinkArtifactKind::PdfSource) {
            return vec![
                HyperlinkArtifactKind::ScreenshotThumbWebp,
                HyperlinkArtifactKind::ScreenshotThumbDarkWebp,
            ];
        }
        return Vec::new();
    }

    vec![
        HyperlinkArtifactKind::ScreenshotWebp,
        HyperlinkArtifactKind::ScreenshotDarkWebp,
        HyperlinkArtifactKind::ScreenshotThumbWebp,
        HyperlinkArtifactKind::ScreenshotThumbDarkWebp,
    ]
}

fn latest_source_artifact_kind(
    latest_artifacts: &HashMap<HyperlinkArtifactKind, hyperlink_artifact::Model>,
) -> Option<HyperlinkArtifactKind> {
    let snapshot = latest_artifacts.get(&HyperlinkArtifactKind::SnapshotWarc);
    let pdf = latest_artifacts.get(&HyperlinkArtifactKind::PdfSource);

    match (snapshot, pdf) {
        (Some(snapshot), Some(pdf)) => {
            if artifact_is_newer(pdf, snapshot) {
                Some(HyperlinkArtifactKind::PdfSource)
            } else {
                Some(HyperlinkArtifactKind::SnapshotWarc)
            }
        }
        (Some(_), None) => Some(HyperlinkArtifactKind::SnapshotWarc),
        (None, Some(_)) => Some(HyperlinkArtifactKind::PdfSource),
        (None, None) => None,
    }
}

fn artifact_is_newer(
    candidate: &hyperlink_artifact::Model,
    current: &hyperlink_artifact::Model,
) -> bool {
    candidate.created_at > current.created_at
        || (candidate.created_at == current.created_at && candidate.id > current.id)
}

fn artifact_kind_info(
    kind: &HyperlinkArtifactKind,
) -> (&'static str, &'static str, &'static str, bool) {
    match kind {
        HyperlinkArtifactKind::SnapshotWarc => ("snapshot_warc", "Snapshot WARC", "warc", false),
        HyperlinkArtifactKind::PdfSource => ("pdf_source", "PDF Source", "pdf", false),
        HyperlinkArtifactKind::PaperlessMetadata => {
            ("paperless_metadata", "Paperless Metadata", "json", false)
        }
        HyperlinkArtifactKind::SnapshotError => ("snapshot_error", "Snapshot Error", "json", true),
        HyperlinkArtifactKind::OembedMeta => ("oembed_meta", "oEmbed Metadata", "json", false),
        HyperlinkArtifactKind::OembedError => ("oembed_error", "oEmbed Error", "json", true),
        HyperlinkArtifactKind::OgMeta => ("og_meta", "Open Graph Metadata", "json", false),
        HyperlinkArtifactKind::OgImage => ("og_image", "Open Graph Image", "img", false),
        HyperlinkArtifactKind::OgError => ("og_error", "Open Graph Error", "json", true),
        HyperlinkArtifactKind::ReadableText => ("readable_text", "Readable Markdown", "md", false),
        HyperlinkArtifactKind::ReadableMeta => {
            ("readable_meta", "Readable Metadata", "json", false)
        }
        HyperlinkArtifactKind::ReadableError => ("readable_error", "Readable Error", "json", true),
        HyperlinkArtifactKind::ScreenshotWebp => {
            ("screenshot_webp", "Screenshot WebP", "webp", false)
        }
        HyperlinkArtifactKind::ScreenshotThumbWebp => (
            "screenshot_thumb_webp",
            "Screenshot Thumbnail",
            "webp",
            false,
        ),
        HyperlinkArtifactKind::ScreenshotDarkWebp => {
            ("screenshot_dark_webp", "Screenshot Dark", "webp", false)
        }
        HyperlinkArtifactKind::ScreenshotThumbDarkWebp => (
            "screenshot_thumb_dark_webp",
            "Screenshot Thumbnail Dark",
            "webp",
            false,
        ),
        HyperlinkArtifactKind::ScreenshotError => {
            ("screenshot_error", "Screenshot Error", "json", true)
        }
        HyperlinkArtifactKind::TagMeta => ("tag_meta", "Tag Metadata", "json", false),
    }
}

fn parse_artifact_kind(value: &str) -> Option<HyperlinkArtifactKind> {
    show_artifact_kinds()
        .into_iter()
        .find(|kind| artifact_kind_info(kind).0 == value)
}

fn artifact_kind_slug(kind: &HyperlinkArtifactKind) -> &'static str {
    artifact_kind_info(kind).0
}

fn artifact_kind_label(kind: &HyperlinkArtifactKind) -> &'static str {
    artifact_kind_info(kind).1
}

fn artifact_kind_file_extension(kind: &HyperlinkArtifactKind) -> &'static str {
    artifact_kind_info(kind).2
}

fn artifact_download_file_extension(
    kind: &HyperlinkArtifactKind,
    artifact: &hyperlink_artifact::Model,
) -> String {
    if *kind == HyperlinkArtifactKind::SnapshotWarc
        && crate::model::hyperlink_artifact::is_snapshot_warc_gzip_artifact(artifact)
    {
        return "warc.gz".to_string();
    }

    artifact_kind_file_extension(kind).to_string()
}

fn artifact_download_path(hyperlink_id: i32, kind: &HyperlinkArtifactKind) -> String {
    format!(
        "/hyperlinks/{hyperlink_id}/artifacts/{}",
        artifact_kind_slug(kind)
    )
}

fn artifact_inline_path(hyperlink_id: i32, kind: &HyperlinkArtifactKind) -> String {
    format!(
        "/hyperlinks/{hyperlink_id}/artifacts/{}/inline",
        artifact_kind_slug(kind)
    )
}

fn artifact_pdf_preview_path(hyperlink_id: i32) -> String {
    format!("/hyperlinks/{hyperlink_id}/artifacts/pdf_source/preview")
}

fn artifact_delete_path(hyperlink_id: i32, kind: &HyperlinkArtifactKind) -> String {
    format!(
        "/hyperlinks/{hyperlink_id}/artifacts/{}/delete",
        artifact_kind_slug(kind)
    )
}

fn artifact_fetch_path(hyperlink_id: i32, kind: &HyperlinkArtifactKind) -> String {
    format!(
        "/hyperlinks/{hyperlink_id}/artifacts/{}/fetch",
        artifact_kind_slug(kind)
    )
}

fn is_readability_artifact_kind(kind: &HyperlinkArtifactKind) -> bool {
    matches!(
        kind,
        HyperlinkArtifactKind::ReadableText
            | HyperlinkArtifactKind::ReadableMeta
            | HyperlinkArtifactKind::ReadableError
    )
}

fn artifact_fetch_job_kind(
    kind: &HyperlinkArtifactKind,
) -> Option<hyperlink_processing_job::HyperlinkProcessingJobKind> {
    match kind {
        HyperlinkArtifactKind::SnapshotWarc
        | HyperlinkArtifactKind::PdfSource
        | HyperlinkArtifactKind::SnapshotError
        | HyperlinkArtifactKind::ScreenshotWebp
        | HyperlinkArtifactKind::ScreenshotThumbWebp
        | HyperlinkArtifactKind::ScreenshotDarkWebp
        | HyperlinkArtifactKind::ScreenshotThumbDarkWebp
        | HyperlinkArtifactKind::ScreenshotError => {
            Some(hyperlink_processing_job::HyperlinkProcessingJobKind::Snapshot)
        }
        HyperlinkArtifactKind::OgMeta
        | HyperlinkArtifactKind::OgImage
        | HyperlinkArtifactKind::OgError => {
            Some(hyperlink_processing_job::HyperlinkProcessingJobKind::Og)
        }
        HyperlinkArtifactKind::ReadableText
        | HyperlinkArtifactKind::ReadableMeta
        | HyperlinkArtifactKind::ReadableError => {
            Some(hyperlink_processing_job::HyperlinkProcessingJobKind::Readability)
        }
        HyperlinkArtifactKind::PaperlessMetadata
        | HyperlinkArtifactKind::OembedMeta
        | HyperlinkArtifactKind::OembedError
        | HyperlinkArtifactKind::TagMeta => None,
    }
}

fn is_screenshot_fetch_artifact_kind(kind: &HyperlinkArtifactKind) -> bool {
    matches!(
        kind,
        HyperlinkArtifactKind::ScreenshotWebp
            | HyperlinkArtifactKind::ScreenshotThumbWebp
            | HyperlinkArtifactKind::ScreenshotDarkWebp
            | HyperlinkArtifactKind::ScreenshotThumbDarkWebp
            | HyperlinkArtifactKind::ScreenshotError
    )
}

fn render_relative_time(datetime: &sea_orm::entity::prelude::DateTime) -> String {
    let datetime_iso = datetime.format("%Y-%m-%dT%H:%M:%SZ");
    let datetime_human = datetime.format("%b %d, %Y %H:%M UTC");
    format!("<relative-time datetime=\"{datetime_iso}\">{datetime_human}</relative-time>")
}

fn format_size_bytes(size_bytes: i32) -> String {
    let bytes = size_bytes.max(0) as f64;
    if bytes < 1024.0 {
        return format!("{}B", bytes as i64);
    }
    if bytes < 1024.0 * 1024.0 {
        return format!("{:.1}KB", bytes / 1024.0);
    }
    format!("{:.1}MB", bytes / (1024.0 * 1024.0))
}

fn response_error(kind: ResponseKind, status: StatusCode, message: impl Into<String>) -> Response {
    let message = message.into();
    match kind {
        ResponseKind::Text => {
            views::render_error_page(status, message, "/hyperlinks", "Back to hyperlinks")
        }
        ResponseKind::Json => json_error(status, message),
    }
}

fn json_error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(ErrorResponse {
            error: message.into(),
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::test_support;
    use axum_test::TestServer;
    use sea_orm::DatabaseConnection;
    use serde::Serialize;
    use serde_json::json;

    async fn new_server() -> TestServer {
        new_server_with_seed(None).await
    }

    async fn new_server_with_seed(seed_sql: Option<&str>) -> TestServer {
        let connection = test_support::new_memory_connection().await;
        test_support::initialize_hyperlinks_schema_with_search(&connection).await;
        test_support::initialize_queue_jobs_schema(&connection).await;
        if let Some(seed_sql) = seed_sql {
            test_support::execute_sql(&connection, seed_sql).await;
        }

        let app = Router::<Context>::new().merge(links()).with_state(Context {
            connection,
            processing_queue: None,
            backup_exports: crate::server::admin_backup::AdminBackupManager::default(),
            backup_imports: crate::server::admin_import::AdminImportManager::default(),
            tag_reclassify: crate::server::admin_tag_reclassify::AdminTagReclassifyManager::default(
            ),
        });
        TestServer::new(app).expect("test server should initialize")
    }

    async fn new_server_with_queue(seed_sql: Option<&str>) -> (TestServer, DatabaseConnection) {
        let connection = test_support::new_memory_connection().await;
        test_support::initialize_hyperlinks_schema_with_search(&connection).await;
        test_support::initialize_queue_jobs_schema(&connection).await;
        if let Some(seed_sql) = seed_sql {
            test_support::execute_sql(&connection, seed_sql).await;
        }

        let queue = crate::queue::ProcessingQueue::connect(connection.clone())
            .await
            .expect("processing queue should initialize");
        let app = Router::<Context>::new().merge(links()).with_state(Context {
            connection: connection.clone(),
            processing_queue: Some(queue),
            backup_exports: crate::server::admin_backup::AdminBackupManager::default(),
            backup_imports: crate::server::admin_import::AdminImportManager::default(),
            tag_reclassify: crate::server::admin_tag_reclassify::AdminTagReclassifyManager::default(
            ),
        });
        (
            TestServer::new(app).expect("test server should initialize"),
            connection,
        )
    }

    #[derive(Serialize)]
    struct HtmlForm<'a> {
        title: &'a str,
        url: &'a str,
    }

    fn form_body(title: &str, url: &str) -> String {
        serde_urlencoded::to_string(HtmlForm { title, url }).expect("form should serialize")
    }

    fn assert_contains_all(body: &str, needles: &[&str]) {
        for needle in needles {
            assert!(body.contains(needle), "missing expected snippet: {needle}");
        }
    }

    fn seed_hyperlinks_insert_sql(count: usize) -> String {
        let mut sql = String::from(
            "INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at) VALUES ",
        );
        for id in 1..=count {
            if id > 1 {
                sql.push_str(", ");
            }
            sql.push_str(&format!(
                "({}, 'Link {}', 'https://example.com/{}', 'https://example.com/{}', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00')",
                id, id, id, id
            ));
        }
        sql.push(';');
        sql
    }

    async fn create_json_hyperlink(
        server: &TestServer,
        title: &str,
        url: &str,
    ) -> HyperlinkResponse {
        let create = server
            .post("/hyperlinks.json")
            .json(&json!({
                "title": title,
                "url": url,
            }))
            .await;
        create.assert_status(StatusCode::CREATED);
        create.json()
    }

    async fn show_json_hyperlink(server: &TestServer, id: i32) -> HyperlinkResponse {
        let show = server.get(&format!("/hyperlinks/{id}.json")).await;
        show.assert_status_ok();
        show.json()
    }

    async fn update_json_hyperlink(
        server: &TestServer,
        id: i32,
        title: &str,
        url: &str,
    ) -> HyperlinkResponse {
        let update = server
            .patch(&format!("/hyperlinks/{id}.json"))
            .json(&json!({
                "title": title,
                "url": url,
            }))
            .await;
        update.assert_status_ok();
        update.json()
    }

    async fn list_json_index(server: &TestServer, query: Option<&str>) -> HyperlinksIndexResponse {
        let path = match query {
            Some(query) => format!("/hyperlinks.json?{query}"),
            None => "/hyperlinks.json".to_string(),
        };
        let list = server.get(&path).await;
        list.assert_status_ok();
        list.json()
    }

    async fn list_json_hyperlinks(server: &TestServer) -> Vec<HyperlinkResponse> {
        list_json_index(server, None).await.items
    }

    async fn lookup_json(server: &TestServer, query: Option<&str>) -> HyperlinkLookupResponse {
        let path = match query {
            Some(query) => format!("/hyperlinks/lookup?{query}"),
            None => "/hyperlinks/lookup".to_string(),
        };
        let response = server.get(&path).await;
        response.assert_status_ok();
        response.json()
    }

    #[tokio::test]
    async fn json_crud_flow_works() {
        let server = new_server().await;

        let created = create_json_hyperlink(&server, "Example", "https://example.com").await;
        assert_eq!(created.title, "Example");
        assert_eq!(created.raw_url, "https://example.com");
        assert_eq!(created.processing_state, "idle");

        let shown = show_json_hyperlink(&server, created.id).await;
        assert_eq!(shown.url, "https://example.com");
        assert_eq!(shown.raw_url, "https://example.com");

        let updated = update_json_hyperlink(
            &server,
            created.id,
            "Updated",
            "https://updated.example.com",
        )
        .await;
        assert_eq!(updated.title, "Updated");

        let delete = server.delete(&format!("/hyperlinks/{}", created.id)).await;
        delete.assert_status_see_other();

        server
            .get(&format!("/hyperlinks/{}.json", created.id))
            .await
            .assert_status_not_found();
    }

    #[tokio::test]
    async fn json_create_autofills_empty_title() {
        let server = new_server().await;

        let created_model = create_json_hyperlink(&server, "", "https://example.com").await;
        assert_eq!(created_model.title, "https://example.com");
        assert_eq!(created_model.raw_url, "https://example.com");

        server
            .post("/hyperlinks.json")
            .json(&json!({
                "title": "Example",
                "url": "   ",
            }))
            .await
            .assert_status_bad_request();
    }

    #[tokio::test]
    async fn json_create_canonicalizes_query_params_and_preserves_raw_url() {
        let server = new_server().await;
        let created = create_json_hyperlink(
            &server,
            "Example",
            "https://example.com/docs?utm_source=newsletter&q=rust&fbclid=abc",
        )
        .await;

        assert_eq!(created.url, "https://example.com/docs?q=rust");
        assert_eq!(
            created.raw_url,
            "https://example.com/docs?utm_source=newsletter&q=rust&fbclid=abc"
        );
    }

    #[tokio::test]
    async fn lookup_returns_invalid_url_without_query_param() {
        let server = new_server().await;
        let response = lookup_json(&server, None).await;
        assert_eq!(response.status, "invalid_url");
        assert!(response.id.is_none());
        assert!(response.canonical_url.is_none());
    }

    #[tokio::test]
    async fn lookup_returns_invalid_url_for_unsupported_scheme() {
        let server = new_server().await;
        let response = lookup_json(&server, Some("url=mailto:test%40example.com")).await;
        assert_eq!(response.status, "invalid_url");
        assert!(response.id.is_none());
        assert!(response.canonical_url.is_none());
    }

    #[tokio::test]
    async fn lookup_returns_not_found_for_valid_url() {
        let server = new_server().await;
        let response = lookup_json(
            &server,
            Some("url=https%3A%2F%2Fexample.com%2Fdocs%3Futm_source%3Dx%26q%3Drust"),
        )
        .await;
        assert_eq!(response.status, "not_found");
        assert!(response.id.is_none());
        assert_eq!(
            response.canonical_url.as_deref(),
            Some("https://example.com/docs?q=rust")
        );
    }

    #[tokio::test]
    async fn lookup_returns_root_for_existing_root_link() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Root', 'https://example.com/root', 'https://example.com/root', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

        let response = lookup_json(&server, Some("url=https%3A%2F%2Fexample.com%2Froot")).await;
        assert_eq!(response.status, "root");
        assert_eq!(response.id, Some(1));
        assert_eq!(
            response.canonical_url.as_deref(),
            Some("https://example.com/root")
        );
    }

    #[tokio::test]
    async fn lookup_returns_discovered_for_existing_discovered_link() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (7, 'Discovered', 'https://example.com/discovered', 'https://example.com/discovered', 1, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

        let response =
            lookup_json(&server, Some("url=https%3A%2F%2Fexample.com%2Fdiscovered")).await;
        assert_eq!(response.status, "discovered");
        assert_eq!(response.id, Some(7));
        assert_eq!(
            response.canonical_url.as_deref(),
            Some("https://example.com/discovered")
        );
    }

    #[tokio::test]
    async fn visit_redirect_increments_click_count() {
        let server = new_server().await;

        let created = create_json_hyperlink(&server, "Example", "https://example.com").await;
        assert_eq!(created.clicks_count, 0);
        assert!(created.last_clicked_at.is_none());

        let visit = server
            .get(&format!("/hyperlinks/{}/visit", created.id))
            .await;
        visit.assert_status(StatusCode::TEMPORARY_REDIRECT);
        visit.assert_header("location", "https://example.com");

        let shown = show_json_hyperlink(&server, created.id).await;
        assert_eq!(shown.clicks_count, 1);
        assert!(shown.last_clicked_at.is_some());
    }

    #[tokio::test]
    async fn click_endpoint_increments_click_count() {
        let server = new_server().await;

        let created = create_json_hyperlink(&server, "Example", "https://example.com").await;
        assert_eq!(created.clicks_count, 0);
        assert!(created.last_clicked_at.is_none());

        let click = server
            .post(&format!("/hyperlinks/{}/click", created.id))
            .await;
        click.assert_status(StatusCode::NO_CONTENT);

        let shown = show_json_hyperlink(&server, created.id).await;
        assert_eq!(shown.clicks_count, 1);
        assert!(shown.last_clicked_at.is_some());
    }

    #[tokio::test]
    async fn html_pages_render() {
        let server = new_server().await;
        let created = create_json_hyperlink(&server, "Example", "https://example.com").await;

        let index = server.get("/hyperlinks").await;
        index.assert_status_ok();
        let index_body = index.text();
        assert_contains_all(
            &index_body,
            &[
                "<!DOCTYPE html>",
                "/hyperlinks/new",
                "href=\"https://example.com\"",
                "data-hyperlink-id=\"1\"",
            ],
        );
        assert!(
            index_body
                .contains("href=\"/hyperlinks/new\" class=\"inline-flex min-h-11 items-center")
        );
        assert!(index_body.contains("data-url-intent-input"));
        assert!(index_body.contains("data-url-intent"));
        assert!(index_body.contains("aria-hidden=\"true\""));
        assert!(index_body.contains("data-url-intent-add-button"));
        assert!(index_body.contains("data-url-intent-root-message"));
        assert!(index_body.contains("You know, you've already saved this link."));
        assert!(index_body.contains("id=\"hyperlinks-url-intent-add-form\""));
        assert!(index_body.contains("data-url-intent-add-form"));
        assert!(index_body.contains("data-url-intent-add-url"));
        assert!(index_body.contains("motion-safe:animate-pulse"));
        assert!(index_body.contains("<details class=\"group sm:hidden\">"));
        assert!(index_body.contains("<summary"));
        assert!(index_body.contains("Filters"));
        assert!(index_body.contains("group-open:rotate-180"));
        assert!(index_body.contains(
            "class=\"hidden sm:flex sm:flex-row sm:flex-nowrap sm:items-center sm:gap-[0.4rem]\""
        ));
        assert!(index_body.contains("data-filter-key=\"status\""));
        assert!(index_body.contains("data-filter-key=\"type\""));
        assert!(index_body.contains("data-filter-key=\"order\""));
        assert!(index_body.contains("data-discovered-filter"));
        assert!(!index_body.contains("id=\"scope-filter\""));
        assert!(index_body.contains("class=\"flex flex-row gap-2 min-w-0 sm:gap-4\""));
        assert!(index_body.contains(&format!("/hyperlinks/{}\">Details", created.id)));
        assert!(!index_body.contains("/hyperlinks/1/visit"));

        let new_page = server.get("/hyperlinks/new").await;
        new_page.assert_status_ok();
        assert!(
            new_page
                .text()
                .contains("action=\"/hyperlinks\" method=\"post\"")
        );

        let show = server.get(&format!("/hyperlinks/{}", created.id)).await;
        show.assert_status_ok();
        let show_body = show.text();
        assert_contains_all(
            &show_body,
            &["Artifacts", "Recent jobs", "Discovered links"],
        );
        assert!(show_body.contains(&format!("/hyperlinks/{}/delete", created.id)));

        let edit = server
            .get(&format!("/hyperlinks/{}/edit", created.id))
            .await;
        edit.assert_status_ok();
        assert!(
            edit.text()
                .contains(&format!("/hyperlinks/{}/update", created.id))
        );
    }

    #[tokio::test]
    async fn show_missing_artifacts_uses_snapshot_source_for_non_pdf_links() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Article', 'https://example.com/article', 'https://example.com/article', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES (1, 1, NULL, 'readable_meta', X'7B7D', 'application/json', 2, '2026-02-19 00:00:01');
            "#,
        ))
        .await;

        let show = server.get("/hyperlinks/1").await;
        show.assert_status_ok();
        let body = show.text();
        assert!(body.contains("Missing:"));
        assert!(body.contains("Snapshot WARC"));
        assert!(body.contains("Readable Markdown"));
        assert!(!body.contains("PDF Source"));
    }

    #[tokio::test]
    async fn show_missing_artifacts_uses_pdf_source_for_pdf_links() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, source_type, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Paper', 'https://example.com/paper.pdf', 'https://example.com/paper.pdf', 'pdf', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES (1, 1, NULL, 'readable_meta', X'7B7D', 'application/json', 2, '2026-02-19 00:00:01');
            "#,
        ))
        .await;

        let show = server.get("/hyperlinks/1").await;
        show.assert_status_ok();
        let body = show.text();
        assert!(body.contains("Missing:"));
        assert!(body.contains("PDF Source"));
        assert!(body.contains("Readable Markdown"));
        assert!(!body.contains("Snapshot WARC"));
        assert!(!body.contains("/hyperlinks/1/artifacts/screenshot_webp/fetch"));
        assert!(!body.contains("/hyperlinks/1/artifacts/screenshot_thumb_webp/fetch"));
        assert!(!body.contains("/hyperlinks/1/artifacts/screenshot_dark_webp/fetch"));
        assert!(!body.contains("/hyperlinks/1/artifacts/screenshot_thumb_dark_webp/fetch"));
    }

    #[tokio::test]
    async fn show_missing_artifacts_requires_pdf_source_for_pdf_links_even_when_snapshot_exists() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, source_type, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Paper', 'https://example.com/paper.pdf', 'https://example.com/paper.pdf', 'pdf', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 1, NULL, 'snapshot_warc', X'57415243', 'application/warc', 4, '2026-02-19 00:00:01'),
                    (2, 1, NULL, 'readable_meta', X'7B7D', 'application/json', 2, '2026-02-19 00:00:02');
            "#,
        ))
        .await;

        let show = server.get("/hyperlinks/1").await;
        show.assert_status_ok();
        let body = show.text();
        assert!(body.contains("Missing:"));
        assert!(body.contains("/hyperlinks/1/artifacts/pdf_source/fetch"));
        assert!(!body.contains("/hyperlinks/1/artifacts/screenshot_webp/fetch"));
        assert!(!body.contains("/hyperlinks/1/artifacts/screenshot_thumb_webp/fetch"));
        assert!(body.contains("/hyperlinks/1/artifacts/readable_text/fetch"));
    }

    #[tokio::test]
    async fn show_missing_artifacts_for_pdf_with_pdf_source_requires_only_thumbnails() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, source_type, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Paper', 'https://example.com/paper.pdf', 'https://example.com/paper.pdf', 'pdf', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 1, NULL, 'pdf_source', X'25504446', 'application/pdf', 4, '2026-02-19 00:00:01'),
                    (2, 1, NULL, 'screenshot_thumb_webp', X'52494646', 'image/webp', 4, '2026-02-19 00:00:02'),
                    (3, 1, NULL, 'readable_meta', X'7B7D', 'application/json', 2, '2026-02-19 00:00:03');
            "#,
        ))
        .await;

        let show = server.get("/hyperlinks/1").await;
        show.assert_status_ok();
        let body = show.text();
        assert!(!body.contains("/hyperlinks/1/artifacts/screenshot_webp/fetch"));
        assert!(!body.contains("/hyperlinks/1/artifacts/screenshot_dark_webp/fetch"));
        assert!(!body.contains("/hyperlinks/1/artifacts/screenshot_thumb_webp/fetch"));
        assert!(body.contains("/hyperlinks/1/artifacts/screenshot_thumb_dark_webp/fetch"));
    }

    #[tokio::test]
    async fn show_missing_artifacts_prefers_existing_pdf_source_for_non_pdf_url() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Article', 'https://example.com/article', 'https://example.com/article', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 1, NULL, 'pdf_source', X'25504446', 'application/pdf', 4, '2026-02-19 00:00:01'),
                    (2, 1, NULL, 'readable_meta', X'7B7D', 'application/json', 2, '2026-02-19 00:00:02');
            "#,
        ))
        .await;

        let show = server.get("/hyperlinks/1").await;
        show.assert_status_ok();
        let body = show.text();
        assert!(body.contains("Missing:"));
        assert!(!body.contains("/hyperlinks/1/artifacts/snapshot_warc/fetch"));
        assert!(body.contains("/hyperlinks/1/artifacts/readable_text/fetch"));
    }

    #[tokio::test]
    async fn show_artifacts_renders_delete_and_fetch_controls() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Article', 'https://example.com/article', 'https://example.com/article', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES (1, 1, NULL, 'readable_meta', X'7B7D', 'application/json', 2, '2026-02-19 00:00:01');
            "#,
        ))
        .await;

        let show = server.get("/hyperlinks/1").await;
        show.assert_status_ok();
        let body = show.text();
        assert!(body.contains("/hyperlinks/1/artifacts/readable_meta/delete"));
        assert!(body.contains("/hyperlinks/1/artifacts/snapshot_warc/fetch"));
        assert!(body.contains("/hyperlinks/1/artifacts/readable_text/fetch"));
    }

    #[tokio::test]
    async fn show_ignores_oembed_title_when_no_open_graph_title() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'https://example.com/watch?v=1', 'https://example.com/watch?v=1', 'https://example.com/watch?v=1', 0, NULL, '2026-02-22 00:00:00', '2026-02-22 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES (
                    1,
                    1,
                    NULL,
                    'oembed_meta',
                    CAST('{"captured_at":"2026-02-22T00:00:30Z","selected":{"title":"Example Video Walkthrough","type":"video","provider_name":"VideoHost","author_name":"Codex Team","thumbnail_url":"https://img.example.com/thumb.jpg","url":"https://cdn.example.com/embed/1"}}' AS BLOB),
                    'application/json',
                    230,
                    '2026-02-22 00:00:31'
                );
            "#,
        ))
        .await;

        let show = server.get("/hyperlinks/1").await;
        show.assert_status_ok();
        let body = show.text();
        assert!(body.contains(">https://example.com/watch?v=1</h2>"));
        assert!(!body.contains("View full oEmbed JSON"));
        assert!(!body.contains(">oEmbed</h4>"));

        let shown = show_json_hyperlink(&server, 1).await;
        assert_eq!(shown.title, "https://example.com/watch?v=1");
    }

    #[tokio::test]
    async fn show_uses_open_graph_title_when_current_title_is_url_like() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (
                    id,
                    title,
                    url,
                    raw_url,
                    og_title,
                    og_description,
                    og_type,
                    og_url,
                    og_image_url,
                    og_site_name,
                    clicks_count,
                    last_clicked_at,
                    created_at,
                    updated_at
                )
                VALUES (
                    1,
                    'https://example.com/watch?v=1',
                    'https://example.com/watch?v=1',
                    'https://example.com/watch?v=1',
                    'Example OG Video',
                    'OG description',
                    'video.other',
                    'https://example.com/watch?v=1',
                    'https://img.example.com/og.jpg',
                    'Example Site',
                    0,
                    NULL,
                    '2026-02-22 00:00:00',
                    '2026-02-22 00:00:00'
                );
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES (
                    1,
                    1,
                    NULL,
                    'og_meta',
                    CAST('{"captured_at":"2026-02-22T00:00:30Z","selected":{"title":"Example OG Video"}}' AS BLOB),
                    'application/json',
                    90,
                    '2026-02-22 00:00:31'
                );
            "#,
        ))
        .await;

        let show = server.get("/hyperlinks/1").await;
        show.assert_status_ok();
        let body = show.text();
        assert!(body.contains(">Example OG Video</h2>"));
        assert!(body.contains("Open Graph</h4>"));
    }

    #[tokio::test]
    async fn show_decodes_html_entities_in_open_graph_fields() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (
                    id,
                    title,
                    url,
                    raw_url,
                    og_title,
                    og_description,
                    og_site_name,
                    clicks_count,
                    last_clicked_at,
                    created_at,
                    updated_at
                )
                VALUES (
                    1,
                    'https://example.com/article',
                    'https://example.com/article',
                    'https://example.com/article',
                    'Cats &amp; Dogs',
                    'Tips &amp; Tricks &#39;Daily&#39;',
                    'News &amp; Co',
                    0,
                    NULL,
                    '2026-02-22 00:00:00',
                    '2026-02-22 00:00:00'
                );
            "#,
        ))
        .await;

        let show = server.get("/hyperlinks/1").await;
        show.assert_status_ok();
        let body = show.text();
        assert!(body.contains(">Cats &amp; Dogs</h2>"));
        assert!(!body.contains(">Cats &amp;amp; Dogs</h2>"));
        assert!(body.contains("Tips &amp; Tricks"));
        assert!(body.contains("Daily"));
        assert!(!body.contains("Tips &amp;amp; Tricks"));
        assert!(!body.contains("&#38;#39;"));
        assert!(!body.contains("&amp;#39;"));
        assert!(body.contains("News &amp; Co"));
        assert!(!body.contains("News &amp;amp; Co"));
    }

    #[tokio::test]
    async fn show_decodes_html_entities_in_fallback_title() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (
                    1,
                    'Cats &amp; Dogs',
                    'https://example.com/article',
                    'https://example.com/article',
                    0,
                    NULL,
                    '2026-02-22 00:00:00',
                    '2026-02-22 00:00:00'
                );
            "#,
        ))
        .await;

        let show = server.get("/hyperlinks/1").await;
        show.assert_status_ok();
        let body = show.text();
        assert!(body.contains(">Cats &amp; Dogs</h2>"));
        assert!(!body.contains(">Cats &amp;amp; Dogs</h2>"));
    }

    #[tokio::test]
    async fn show_renders_dark_mode_aware_screenshot_when_artifacts_exist() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (
                    1,
                    'Example article',
                    'https://example.com/article',
                    'https://example.com/article',
                    0,
                    NULL,
                    '2026-02-22 00:00:00',
                    '2026-02-22 00:00:00'
                );
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 1, NULL, 'screenshot_webp', X'00', 'image/webp', 1, '2026-02-22 00:00:01'),
                    (2, 1, NULL, 'screenshot_dark_webp', X'00', 'image/webp', 1, '2026-02-22 00:00:02');
                INSERT INTO hyperlink_processing_job (id, hyperlink_id, kind, state, error_message, queued_at, started_at, finished_at, created_at, updated_at)
                VALUES
                    (42, 1, 'snapshot', 'succeeded', NULL, '2026-02-22 00:00:03', '2026-02-22 00:00:04', '2026-02-22 00:00:05', '2026-02-22 00:00:03', '2026-02-22 00:00:05');
            "#,
        ))
        .await;

        let show = server.get("/hyperlinks/1").await;
        show.assert_status_ok();
        let body = show.text();
        assert!(body.contains("/hyperlinks/1/artifacts/screenshot_webp/inline"));
        assert!(body.contains("/hyperlinks/1/artifacts/screenshot_dark_webp/inline"));
        assert!(body.contains("media=\"(prefers-color-scheme: dark)\""));
        assert!(body.contains("Screenshot for Example article"));
        assert!(body.contains("break-all text-sm"));
        assert!(body.contains("class=\"flex flex-row flex-wrap gap-4 text-sm\""));
        assert!(
            body.contains("class=\"flex flex-col gap-2 sm:flex-row sm:items-center sm:gap-4\"")
        );
        assert!(body.contains("job#42"));
        assert!(body.matches("class=\"overflow-x-auto\"").count() >= 2);
    }

    #[tokio::test]
    async fn show_renders_pdf_iframe_and_skips_screenshot_preview_for_pdf_sources() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, source_type, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (
                    1,
                    'Example paper',
                    'https://example.com/paper.pdf',
                    'https://example.com/paper.pdf',
                    'pdf',
                    0,
                    NULL,
                    '2026-02-22 00:00:00',
                    '2026-02-22 00:00:00'
                );
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 1, NULL, 'pdf_source', X'25504446', 'application/pdf', 4, '2026-02-22 00:00:01'),
                    (2, 1, NULL, 'screenshot_webp', X'00', 'image/webp', 1, '2026-02-22 00:00:02'),
                    (3, 1, NULL, 'screenshot_dark_webp', X'00', 'image/webp', 1, '2026-02-22 00:00:03');
            "#,
        ))
        .await;

        let show = server.get("/hyperlinks/1").await;
        show.assert_status_ok();
        let body = show.text();
        assert!(body.contains("<iframe"));
        assert!(body.contains("PDF preview for Example paper"));
        assert!(body.contains("/hyperlinks/1/artifacts/pdf_source/preview"));
        assert!(!body.contains("Screenshot for Example paper"));
    }

    #[tokio::test]
    async fn index_decodes_html_entities_in_link_title_card() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (
                    1,
                    'Cats &amp; Dogs',
                    'https://example.com/article',
                    'https://example.com/article',
                    0,
                    NULL,
                    '2026-02-22 00:00:00',
                    '2026-02-22 00:00:00'
                );
            "#,
        ))
        .await;

        let index = server.get("/hyperlinks").await;
        index.assert_status_ok();
        let body = index.text();
        assert!(body.contains("Cats &amp; Dogs"));
        assert!(!body.contains("Cats &amp;amp; Dogs"));
    }

    #[tokio::test]
    async fn show_decodes_html_entities_in_discovered_link_title_card() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Parent', 'https://example.com/parent', 'https://example.com/parent', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Cats &amp; Dogs', 'https://example.com/child', 'https://example.com/child', 1, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
                INSERT INTO hyperlink_relation (id, parent_hyperlink_id, child_hyperlink_id, created_at)
                VALUES (1, 1, 2, '2026-02-19 00:00:02');
            "#,
        ))
        .await;

        let show = server.get("/hyperlinks/1").await;
        show.assert_status_ok();
        let body = show.text();
        assert!(body.contains("Cats &amp; Dogs"));
        assert!(!body.contains("Cats &amp;amp; Dogs"));
    }

    #[tokio::test]
    async fn show_prefers_open_graph_block_over_oembed_block_when_both_exist() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (
                    id,
                    title,
                    url,
                    raw_url,
                    og_title,
                    og_description,
                    og_type,
                    og_url,
                    og_image_url,
                    og_site_name,
                    clicks_count,
                    last_clicked_at,
                    created_at,
                    updated_at
                )
                VALUES (
                    1,
                    'https://example.com/watch?v=1',
                    'https://example.com/watch?v=1',
                    'https://example.com/watch?v=1',
                    'Example OG Video',
                    'OG description',
                    'video.other',
                    'https://example.com/watch?v=1',
                    'https://img.example.com/og.jpg',
                    'Example Site',
                    0,
                    NULL,
                    '2026-02-22 00:00:00',
                    '2026-02-22 00:00:00'
                );
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (
                        1,
                        1,
                        NULL,
                        'og_meta',
                        CAST('{"captured_at":"2026-02-22T00:00:30Z","selected":{"title":"Example OG Video","description":"OG description"}}' AS BLOB),
                        'application/json',
                        120,
                        '2026-02-22 00:00:31'
                    ),
                    (
                        2,
                        1,
                        NULL,
                        'oembed_meta',
                        CAST('{"captured_at":"2026-02-22T00:00:30Z","selected":{"title":"Example oEmbed Video","type":"video","provider_name":"VideoHost"}}' AS BLOB),
                        'application/json',
                        140,
                        '2026-02-22 00:00:31'
                    );
            "#,
        ))
        .await;

        let show = server.get("/hyperlinks/1").await;
        show.assert_status_ok();
        let body = show.text();
        assert!(body.contains("Open Graph</h4>"));
        assert!(body.contains("View full Open Graph JSON"));
        assert!(!body.contains(">oEmbed</h4>"));
        assert!(!body.contains("View full oEmbed JSON"));
    }

    #[tokio::test]
    async fn index_failed_status_shows_failed_badge() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example', 'https://example.com', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_processing_job (id, hyperlink_id, kind, state, error_message, queued_at, started_at, finished_at, created_at, updated_at)
                VALUES (42, 1, 'snapshot', 'failed', 'snapshot request failed', '2026-02-19 00:01:00', '2026-02-19 00:01:10', '2026-02-19 00:01:20', '2026-02-19 00:01:00', '2026-02-19 00:01:20');
            "#,
        ))
        .await;

        let index = server.get("/hyperlinks").await;
        index.assert_status_ok();
        let body = index.text();
        assert_contains_all(&body, &["Failed"]);
        assert!(!body.contains("/jobs/42"));
    }

    #[tokio::test]
    async fn artifact_download_endpoint_uses_latest_artifact_per_kind() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example', 'https://example.com', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 1, NULL, 'readable_text', X'6669727374', 'text/markdown; charset=utf-8', 5, '2026-02-19 00:00:01'),
                    (2, 1, NULL, 'readable_text', X'7365636f6e642070726576696577', 'text/markdown; charset=utf-8', 14, '2026-02-19 00:00:02'),
                    (3, 1, NULL, 'pdf_source', X'255044462D312E34', 'application/pdf', 8, '2026-02-19 00:00:01'),
                    (4, 1, NULL, 'pdf_source', X'255044462D312E350A25', 'application/pdf', 10, '2026-02-19 00:00:03'),
                    (5, 1, NULL, 'snapshot_warc', X'57415243', 'application/warc', 4, '2026-02-19 00:00:04'),
                    (6, 1, NULL, 'snapshot_warc', X'1F8B0800920EA06900030B770C7256284ECC2DC8490500D757B83F0B000000', 'application/warc+gzip', 31, '2026-02-19 00:00:05');
            "#,
        ))
        .await;

        let download = server.get("/hyperlinks/1/artifacts/readable_text").await;
        download.assert_status_ok();
        download.assert_header("content-type", "text/markdown; charset=utf-8");
        download.assert_header(
            "content-disposition",
            "attachment; filename=\"hyperlink-1-readable_text.md\"",
        );
        assert_eq!(download.text(), "second preview");

        let pdf_download = server.get("/hyperlinks/1/artifacts/pdf_source").await;
        pdf_download.assert_status_ok();
        pdf_download.assert_header("content-type", "application/pdf");
        pdf_download.assert_header(
            "content-disposition",
            "attachment; filename=\"hyperlink-1-pdf_source.pdf\"",
        );
        assert_eq!(pdf_download.text(), "%PDF-1.5\n%");

        let inline = server
            .get("/hyperlinks/1/artifacts/readable_text/inline")
            .await;
        inline.assert_status_ok();
        inline.assert_header("content-type", "text/markdown; charset=utf-8");
        assert_eq!(inline.text(), "second preview");

        let pdf_inline = server
            .get("/hyperlinks/1/artifacts/pdf_source/inline")
            .await;
        pdf_inline.assert_status_ok();
        pdf_inline.assert_header("content-type", "application/pdf");
        assert_eq!(pdf_inline.text(), "%PDF-1.5\n%");

        let pdf_preview = server
            .get("/hyperlinks/1/artifacts/pdf_source/preview")
            .await;
        pdf_preview.assert_status_ok();
        pdf_preview.assert_header("content-type", "text/html; charset=utf-8");
        let pdf_preview_body = pdf_preview.text();
        assert!(pdf_preview_body.contains("color-scheme: light;"));
        assert!(
            pdf_preview_body.contains(
                "<embed src=\"/hyperlinks/1/artifacts/pdf_source/inline#zoom=page-width\""
            )
        );

        let warc_download = server.get("/hyperlinks/1/artifacts/snapshot_warc").await;
        warc_download.assert_status_ok();
        warc_download.assert_header("content-type", "application/warc+gzip");
        warc_download.assert_header(
            "content-disposition",
            "attachment; filename=\"hyperlink-1-snapshot_warc.warc.gz\"",
        );
        assert!(warc_download.as_bytes().starts_with(&[0x1f, 0x8b]));

        let warc_inline = server
            .get("/hyperlinks/1/artifacts/snapshot_warc/inline")
            .await;
        warc_inline.assert_status_ok();
        warc_inline.assert_header("content-type", "application/warc+gzip");
        assert!(warc_inline.as_bytes().starts_with(&[0x1f, 0x8b]));

        server
            .get("/hyperlinks/1/artifacts/not_a_kind")
            .await
            .assert_status_bad_request();
        server
            .get("/hyperlinks/999/artifacts/readable_text")
            .await
            .assert_status_not_found();
    }

    #[tokio::test]
    async fn artifact_download_endpoint_keeps_legacy_snapshot_warc_extension_for_plain_content_type()
     {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example', 'https://example.com', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 1, NULL, 'snapshot_warc', X'57415243', 'application/warc', 4, '2026-02-19 00:00:01');
            "#,
        ))
        .await;

        let download = server.get("/hyperlinks/1/artifacts/snapshot_warc").await;
        download.assert_status_ok();
        download.assert_header("content-type", "application/warc");
        download.assert_header(
            "content-disposition",
            "attachment; filename=\"hyperlink-1-snapshot_warc.warc\"",
        );
        assert_eq!(download.text(), "WARC");
    }

    #[tokio::test]
    async fn delete_artifact_kind_removes_all_rows_for_that_kind() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example', 'https://example.com', 'https://example.com', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 1, NULL, 'readable_text', X'6669727374', 'text/markdown; charset=utf-8', 5, '2026-02-19 00:00:01'),
                    (2, 1, NULL, 'readable_text', X'7365636f6e64', 'text/markdown; charset=utf-8', 6, '2026-02-19 00:00:02');
            "#,
        ))
        .await;

        let delete = server
            .post("/hyperlinks/1/artifacts/readable_text/delete")
            .await;
        delete.assert_status_see_other();
        delete.assert_header("location", "/hyperlinks/1");

        server
            .get("/hyperlinks/1/artifacts/readable_text")
            .await
            .assert_status_not_found();
    }

    #[tokio::test]
    async fn delete_readability_artifact_clears_search_doc_text() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example', 'https://example.com', 'https://example.com', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES (1, 1, NULL, 'readable_text', CAST('quantumneedle appears here' AS BLOB), 'text/markdown; charset=utf-8', 24, '2026-02-19 00:00:01');
            "#,
        ))
        .await;

        let before = server.get("/hyperlinks?q=quantumneedle").await;
        before.assert_status_ok();
        assert!(before.text().contains("/hyperlinks/1\">Details"));

        let delete = server
            .post("/hyperlinks/1/artifacts/readable_text/delete")
            .await;
        delete.assert_status_see_other();

        let after = server.get("/hyperlinks?q=quantumneedle").await;
        after.assert_status_ok();
        assert!(!after.text().contains("/hyperlinks/1\">Details"));
    }

    #[tokio::test]
    async fn fetch_artifact_kind_enqueues_snapshot_job() {
        let (server, connection) = new_server_with_queue(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example', 'https://example.com', 'https://example.com', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

        let fetch = server
            .post("/hyperlinks/1/artifacts/screenshot_webp/fetch")
            .await;
        fetch.assert_status_see_other();
        fetch.assert_header("location", "/hyperlinks/1");

        let latest = crate::model::hyperlink_processing_job::latest_for_hyperlink(&connection, 1)
            .await
            .expect("latest job should load")
            .expect("job should exist");
        assert_eq!(
            latest.kind,
            hyperlink_processing_job::HyperlinkProcessingJobKind::Snapshot
        );
    }

    #[tokio::test]
    async fn fetch_artifact_kind_blocks_pdf_thumbnail_fetch_until_pdf_source_exists() {
        let (server, connection) = new_server_with_queue(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, source_type, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Uploaded PDF', '/uploads/1/paper.pdf', '/uploads/1/paper.pdf', 'pdf', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

        let fetch = server
            .post("/hyperlinks/1/artifacts/screenshot_thumb_webp/fetch")
            .await;
        fetch.assert_status_see_other();
        fetch.assert_header("location", "/hyperlinks/1");

        let latest = crate::model::hyperlink_processing_job::latest_for_hyperlink(&connection, 1)
            .await
            .expect("latest job query should succeed");
        assert!(latest.is_none());
    }

    #[tokio::test]
    async fn fetch_artifact_kind_enqueues_og_and_readability_jobs() {
        let (server, connection) = new_server_with_queue(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example', 'https://example.com', 'https://example.com', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

        let og_fetch = server.post("/hyperlinks/1/artifacts/og_meta/fetch").await;
        og_fetch.assert_status_see_other();

        let og_latest =
            crate::model::hyperlink_processing_job::latest_for_hyperlink(&connection, 1)
                .await
                .expect("latest job should load")
                .expect("job should exist");
        assert_eq!(
            og_latest.kind,
            hyperlink_processing_job::HyperlinkProcessingJobKind::Og
        );

        let readable_fetch = server
            .post("/hyperlinks/1/artifacts/readable_text/fetch")
            .await;
        readable_fetch.assert_status_see_other();

        let readable_latest =
            crate::model::hyperlink_processing_job::latest_for_hyperlink(&connection, 1)
                .await
                .expect("latest job should load")
                .expect("job should exist");
        assert_eq!(
            readable_latest.kind,
            hyperlink_processing_job::HyperlinkProcessingJobKind::Readability
        );
    }

    #[tokio::test]
    async fn fetch_artifact_kind_without_queue_keeps_processing_idle() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example', 'https://example.com', 'https://example.com', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

        let fetch = server
            .post("/hyperlinks/1/artifacts/readable_text/fetch")
            .await;
        fetch.assert_status_see_other();
        fetch.assert_header("location", "/hyperlinks/1");

        let shown = show_json_hyperlink(&server, 1).await;
        assert_eq!(shown.processing_state, "idle");
    }

    #[tokio::test]
    async fn html_write_flows_redirect() {
        let server = new_server().await;

        let create = server
            .post("/hyperlinks")
            .text(form_body("Example", "https://example.com"))
            .content_type("application/x-www-form-urlencoded")
            .await;
        create.assert_status_see_other();
        create.assert_header("location", "/hyperlinks/1");

        let reprocess = server.post("/hyperlinks/1/reprocess").await;
        reprocess.assert_status_see_other();
        reprocess.assert_header("location", "/hyperlinks/1");

        let update = server
            .post("/hyperlinks/1/update")
            .text(form_body("Updated", "https://updated.example.com"))
            .content_type("application/x-www-form-urlencoded")
            .await;
        update.assert_status_see_other();
        update.assert_header("location", "/hyperlinks/1");

        let delete = server.post("/hyperlinks/1/delete").await;
        delete.assert_status_see_other();
        delete.assert_header("location", "/hyperlinks");

        server
            .get("/hyperlinks/1.json")
            .await
            .assert_status_not_found();
    }

    #[tokio::test]
    async fn index_hides_discovered_links_and_show_displays_them() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Parent', 'https://example.com/parent', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Child', 'https://example.com/child', 1, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
                INSERT INTO hyperlink_relation (id, parent_hyperlink_id, child_hyperlink_id, created_at)
                VALUES (1, 1, 2, '2026-02-19 00:00:02');
            "#,
        ))
        .await;

        let index = server.get("/hyperlinks").await;
        index.assert_status_ok();
        let index_body = index.text();
        assert!(index_body.contains("Parent"));
        assert!(!index_body.contains("Child"));

        let listed = list_json_hyperlinks(&server).await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "Parent");

        let show = server.get("/hyperlinks/1").await;
        show.assert_status_ok();
        let show_body = show.text();
        assert_contains_all(&show_body, &["Discovered links", "Child", "/hyperlinks/2"]);
    }

    #[tokio::test]
    async fn direct_add_promotes_discovered_link_to_root() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Discovered', 'https://example.com/child', 1, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

        let promoted =
            create_json_hyperlink(&server, "Added Directly", "https://example.com/child").await;
        assert_eq!(promoted.id, 1);
        assert_eq!(promoted.title, "Added Directly");

        let listed = list_json_hyperlinks(&server).await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, 1);
        assert_eq!(listed[0].title, "Added Directly");
    }

    #[tokio::test]
    async fn index_query_scope_all_includes_discovered_links_in_html_and_json() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Root', 'https://example.com/root', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Discovered', 'https://example.com/discovered', 1, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
            "#,
        ))
        .await;

        let html = server.get("/hyperlinks?q=scope:all").await;
        html.assert_status_ok();
        let html_body = html.text();
        assert!(html_body.contains("Root"));
        assert!(html_body.contains("Discovered"));

        let json = list_json_index(&server, Some("q=scope:all")).await;
        let titles = json
            .items
            .iter()
            .map(|item| item.title.as_str())
            .collect::<Vec<_>>();
        assert_eq!(titles.len(), 2);
        assert!(titles.contains(&"Root"));
        assert!(titles.contains(&"Discovered"));
    }

    #[tokio::test]
    async fn index_query_with_discovered_includes_discovered_links_in_html_and_json() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Root', 'https://example.com/root', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Discovered', 'https://example.com/discovered', 1, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
            "#,
        ))
        .await;

        let html = server.get("/hyperlinks?q=with:discovered").await;
        html.assert_status_ok();
        let html_body = html.text();
        assert!(html_body.contains("Root"));
        assert!(html_body.contains("Discovered"));
        assert!(html_body.contains("<details class=\"group sm:hidden\" open>"));
        assert!(html_body.contains("data-discovered-filter"));
        assert!(html_body.contains("checked"));

        let json = list_json_index(&server, Some("q=with:discovered")).await;
        let titles = json
            .items
            .iter()
            .map(|item| item.title.as_str())
            .collect::<Vec<_>>();
        assert_eq!(titles.len(), 2);
        assert!(titles.contains(&"Root"));
        assert!(titles.contains(&"Discovered"));
    }

    #[tokio::test]
    async fn index_query_status_failed_filters_by_latest_processing_job_state() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Failed', 'https://example.com/failed', 'https://example.com/failed', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Processing', 'https://example.com/processing', 'https://example.com/processing', 0, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01'),
                    (3, 'Idle', 'https://example.com/idle', 'https://example.com/idle', 0, 0, NULL, '2026-02-19 00:00:02', '2026-02-19 00:00:02');
                INSERT INTO hyperlink_processing_job (id, hyperlink_id, kind, state, error_message, queued_at, started_at, finished_at, created_at, updated_at)
                VALUES
                    (10, 1, 'snapshot', 'failed', 'failed', '2026-02-19 00:01:00', '2026-02-19 00:01:10', '2026-02-19 00:01:20', '2026-02-19 00:01:00', '2026-02-19 00:01:20'),
                    (11, 2, 'snapshot', 'running', NULL, '2026-02-19 00:02:00', '2026-02-19 00:02:10', NULL, '2026-02-19 00:02:00', '2026-02-19 00:02:10');
            "#,
        ))
        .await;

        let json = list_json_index(&server, Some("q=status:failed")).await;
        assert_eq!(json.items.len(), 1);
        assert_eq!(json.items[0].title, "Failed");

        let html = server.get("/hyperlinks?q=status:failed").await;
        html.assert_status_ok();
        let body = html.text();
        assert!(body.contains("Failed"));
        assert!(!body.contains("https://example.com/processing"));
        assert!(!body.contains("https://example.com/idle"));
    }

    #[tokio::test]
    async fn index_query_returns_diagnostics_and_random_order_selection() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'One', 'https://example.com/1', 'https://example.com/1', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Two', 'https://example.com/2', 'https://example.com/2', 0, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
            "#,
        ))
        .await;

        let response = list_json_index(&server, Some("q=order:random+status:not-real")).await;
        assert_eq!(response.items.len(), 2);
        assert_eq!(response.query.raw_q, "order:random status:not-real");
        assert_eq!(response.query.parsed.orders.len(), 1);
        assert_eq!(
            response.query.parsed.orders[0],
            crate::server::hyperlink_fetcher::OrderToken::Random
        );
        assert_eq!(
            response.query.ignored_tokens,
            vec!["status:not-real".to_string()]
        );
        assert!(response.query.free_text.is_empty());
    }

    #[tokio::test]
    async fn index_hides_relevance_sort_option_without_free_text() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Article', 'https://example.com/article', 'https://example.com/article', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

        let html = server.get("/hyperlinks").await;
        html.assert_status_ok();
        let body = html.text();
        assert!(!body.contains("value=\"relevance\""));
    }

    #[tokio::test]
    async fn index_shows_relevance_sort_option_with_free_text() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Rust article', 'https://example.com/rust', 'https://example.com/rust', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

        let html = server.get("/hyperlinks?q=rust").await;
        html.assert_status_ok();
        let body = html.text();
        assert!(body.contains("value=\"relevance\""));
    }

    #[tokio::test]
    async fn index_shows_newest_sort_option_as_default() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Article', 'https://example.com/article', 'https://example.com/article', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

        let html = server.get("/hyperlinks").await;
        html.assert_status_ok();
        let body = html.text();
        assert!(!body.contains("<option value=\"\">Status</option>"));
        assert!(!body.contains("<option value=\"\">Type</option>"));
        assert!(!body.contains("<option value=\"\">Sort</option>"));
        assert!(!body.contains("id=\"scope-filter\""));
        assert!(body.contains("data-discovered-filter"));
        assert!(body.contains("value=\"all\" selected"));
        assert!(body.contains("value=\"newest\""));
        assert!(body.contains("value=\"newest\" selected"));
    }

    #[tokio::test]
    async fn index_shows_no_matches_copy_when_filters_exclude_all_links() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Article', 'https://example.com/article', 'https://example.com/article', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

        let html = server.get("/hyperlinks?q=type:pdf").await;
        html.assert_status_ok();
        let body = html.text();
        assert!(body.contains("No hyperlinks match the current filters."));
        assert!(!body.contains("No hyperlinks yet."));
    }

    #[tokio::test]
    async fn index_query_type_pdf_uses_source_type_not_url_or_artifact_fallbacks() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, source_type, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'ArXiv', 'https://arxiv.org/pdf/2602.11988', 'https://arxiv.org/pdf/2602.11988', 'pdf', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Pdf Suffix Html', 'https://example.com/report.pdf', 'https://example.com/report.pdf', 'html', 0, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01'),
                    (3, 'Pending Pdf', 'https://example.com/pending.pdf', 'https://example.com/pending.pdf', 'pdf', 0, 0, NULL, '2026-02-19 00:00:02', '2026-02-19 00:00:02'),
                    (4, 'Article', 'https://example.com/article', 'https://example.com/article', 'html', 0, 0, NULL, '2026-02-19 00:00:03', '2026-02-19 00:00:03');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (10, 1, NULL, 'snapshot_warc', X'57415243', 'application/warc', 4, '2026-02-19 00:00:04'),
                    (11, 2, NULL, 'pdf_source', X'25504446', 'application/pdf', 4, '2026-02-19 00:00:05');
            "#,
        ))
        .await;

        let pdf = list_json_index(&server, Some("q=type:pdf")).await;
        let pdf_titles = pdf
            .items
            .iter()
            .map(|item| item.title.as_str())
            .collect::<Vec<_>>();
        assert_eq!(pdf_titles.len(), 2, "pdf titles: {:?}", pdf_titles);
        assert!(pdf_titles.contains(&"ArXiv"));
        assert!(pdf_titles.contains(&"Pending Pdf"));
        assert!(!pdf_titles.contains(&"Pdf Suffix Html"));

        let non_pdf = list_json_index(&server, Some("q=type:non-pdf")).await;
        let non_pdf_titles = non_pdf
            .items
            .iter()
            .map(|item| item.title.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            non_pdf_titles.len(),
            2,
            "non-pdf titles: {:?}",
            non_pdf_titles
        );
        assert!(non_pdf_titles.contains(&"Pdf Suffix Html"));
        assert!(non_pdf_titles.contains(&"Article"));
        assert!(!non_pdf_titles.contains(&"ArXiv"));
    }

    #[tokio::test]
    async fn index_renders_pdf_badge_for_pdf_source_type_without_pdf_suffix() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, source_type, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'ArXiv', 'https://arxiv.org/pdf/2602.11988', 'https://arxiv.org/pdf/2602.11988', 'pdf', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

        let html = server.get("/hyperlinks").await;
        html.assert_status_ok();
        let body = html.text();
        assert!(body.contains("ArXiv"));
        assert!(body.contains("PDF</span>"));
    }

    #[tokio::test]
    async fn index_uses_dark_thumbnail_artifacts_without_frontend_pdf_filter() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, source_type, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'PDF Link', 'https://arxiv.org/pdf/2602.11988', 'https://arxiv.org/pdf/2602.11988', 'pdf', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'HTML Link', 'https://example.com/article', 'https://example.com/article', 'html', 0, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (10, 1, NULL, 'pdf_source', X'25504446', 'application/pdf', 4, '2026-02-19 00:00:02'),
                    (11, 1, NULL, 'screenshot_thumb_webp', X'52494646', 'image/webp', 4, '2026-02-19 00:00:03'),
                    (12, 1, NULL, 'screenshot_thumb_dark_webp', X'52494646', 'image/webp', 4, '2026-02-19 00:00:04'),
                    (13, 2, NULL, 'screenshot_thumb_webp', X'52494646', 'image/webp', 4, '2026-02-19 00:00:05');
            "#,
        ))
        .await;

        let html = server.get("/hyperlinks").await;
        html.assert_status_ok();
        let body = html.text();

        assert!(body.contains("/hyperlinks/1/artifacts/screenshot_thumb_dark_webp/inline"));
        assert!(body.contains("media=\"(prefers-color-scheme: dark)\""));
        assert!(!body.contains("pdf-thumbnail-neutral-invert"));
        assert!(!body.contains("id=\"pdf-neutral-invert\""));
    }

    #[tokio::test]
    async fn index_query_free_text_matches_readable_text_content() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Alpha', 'https://example.com/a', 'https://example.com/a', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Beta', 'https://example.com/b', 'https://example.com/b', 0, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (10, 1, NULL, 'readable_text', CAST('rust systems guide' AS BLOB), 'text/markdown; charset=utf-8', 18, '2026-02-19 00:00:02'),
                    (11, 2, NULL, 'readable_text', CAST('python scripting notes' AS BLOB), 'text/markdown; charset=utf-8', 22, '2026-02-19 00:00:03');
            "#,
        ))
        .await;

        let response = list_json_index(&server, Some("q=rust")).await;
        assert_eq!(response.items.len(), 1);
        assert_eq!(response.items[0].title, "Alpha");
    }

    #[tokio::test]
    async fn index_query_free_text_renders_match_snippet_in_html() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Alpha', 'https://example.com/a', 'https://example.com/a', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Beta', 'https://example.com/rust-link', 'https://example.com/rust-link', 0, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (10, 1, NULL, 'readable_text', CAST('this readable text mentions rust and systems' AS BLOB), 'text/markdown; charset=utf-8', 44, '2026-02-19 00:00:02');
            "#,
        ))
        .await;

        let html = server.get("/hyperlinks?q=rust").await;
        html.assert_status_ok();
        let body = html.text();
        assert!(body.contains("this readable text mentions <em>rust</em> and systems"));
        assert!(body.contains("https://example.com/<em>rust</em>-link"));
    }

    #[tokio::test]
    async fn index_query_quoted_term_matches_exact_word_only() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Parsers guide', 'https://example.com/parsers', 'https://example.com/parsers', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Parser guide', 'https://example.com/parser', 'https://example.com/parser', 0, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
            "#,
        ))
        .await;

        let response = list_json_index(&server, Some("q=%22parser%22")).await;
        assert_eq!(response.items.len(), 1);
        assert_eq!(response.items[0].title, "Parser guide");

        let html = server.get("/hyperlinks?q=%22parser%22").await;
        html.assert_status_ok();
        let body = html.text();
        assert!(body.contains("Parser guide"));
        assert!(!body.contains("Parsers guide"));
    }

    #[tokio::test]
    async fn index_query_free_text_falls_back_to_title_url_for_missing_readability() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Rust no readability', 'https://example.com/rust-no-readability', 'https://example.com/rust-no-readability', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Rust with readability mismatch', 'https://example.com/rust-with-readability', 'https://example.com/rust-with-readability', 0, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (10, 2, NULL, 'readable_text', CAST('python only body' AS BLOB), 'text/markdown; charset=utf-8', 16, '2026-02-19 00:00:02');
            "#,
        ))
        .await;

        let response = list_json_index(&server, Some("q=rust")).await;
        assert_eq!(response.items.len(), 2);
        let ids = response
            .items
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>();
        assert!(ids.contains(&1));
        assert!(ids.contains(&2));
    }

    #[tokio::test]
    async fn index_query_order_relevance_without_text_falls_back_to_newest() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Older', 'https://example.com/older', 'https://example.com/older', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Newer', 'https://example.com/newer', 'https://example.com/newer', 0, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
            "#,
        ))
        .await;

        let response = list_json_index(&server, Some("q=order:relevance")).await;
        assert_eq!(response.items.len(), 2);
        assert_eq!(response.items[0].title, "Newer");
        assert_eq!(response.items[1].title, "Older");
    }

    #[tokio::test]
    async fn index_query_explicit_order_overrides_default_relevance_ordering() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Older', 'https://example.com/older', 'https://example.com/older', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Newer', 'https://example.com/newer', 'https://example.com/newer', 0, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (10, 1, NULL, 'readable_text', CAST('rust article' AS BLOB), 'text/markdown; charset=utf-8', 12, '2026-02-19 00:00:02'),
                    (11, 2, NULL, 'readable_text', CAST('rust article' AS BLOB), 'text/markdown; charset=utf-8', 12, '2026-02-19 00:00:03');
            "#,
        ))
        .await;

        let response = list_json_index(&server, Some("q=rust+order:oldest")).await;
        assert_eq!(response.items.len(), 2);
        assert_eq!(response.items[0].title, "Older");
        assert_eq!(response.items[1].title, "Newer");
    }

    #[tokio::test]
    async fn index_json_paginates_100_per_page() {
        let seed_sql = seed_hyperlinks_insert_sql(205);
        let server = new_server_with_seed(Some(seed_sql.as_str())).await;

        let page_1 = list_json_index(&server, None).await;
        assert_eq!(page_1.items.len(), 100);
        assert_eq!(page_1.items[0].id, 205);
        assert_eq!(page_1.items[99].id, 106);

        let page_2 = list_json_index(&server, Some("page=2")).await;
        assert_eq!(page_2.items.len(), 100);
        assert_eq!(page_2.items[0].id, 105);
        assert_eq!(page_2.items[99].id, 6);

        let page_3 = list_json_index(&server, Some("page=3")).await;
        assert_eq!(page_3.items.len(), 5);
        assert_eq!(page_3.items[0].id, 5);
        assert_eq!(page_3.items[4].id, 1);

        let clamped = list_json_index(&server, Some("page=99")).await;
        assert_eq!(clamped.items.len(), 5);
        assert_eq!(clamped.items[0].id, 5);
        assert_eq!(clamped.items[4].id, 1);
    }

    #[tokio::test]
    async fn index_html_renders_pagination_links_and_preserves_query() {
        let seed_sql = seed_hyperlinks_insert_sql(101);
        let server = new_server_with_seed(Some(seed_sql.as_str())).await;

        let first_page = server.get("/hyperlinks?q=link").await;
        first_page.assert_status_ok();
        let first_body = first_page.text();
        assert!(first_body.contains("Page 1 of 2"));
        assert!(first_body.contains("/hyperlinks?q=link&amp;page=2"));
        assert!(first_body.contains("Details"));

        let second_page = server.get("/hyperlinks?q=link&page=2").await;
        second_page.assert_status_ok();
        let second_body = second_page.text();
        assert!(second_body.contains("Page 2 of 2"));
        assert!(second_body.contains("/hyperlinks?q=link&amp;page=1"));
        assert!(second_body.contains("/hyperlinks/1\">Details"));
        assert!(!second_body.contains("/hyperlinks/101\">Details"));

        let clamped_page = server.get("/hyperlinks?q=link&page=99").await;
        clamped_page.assert_status_ok();
        let clamped_body = clamped_page.text();
        assert!(clamped_body.contains("Page 2 of 2"));
        assert!(clamped_body.contains("/hyperlinks?q=link&amp;page=1"));
    }

    #[tokio::test]
    async fn index_json_cleans_site_suffix_titles() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Understanding Rust Lifetimes | Example.com', 'https://example.com/rust/lifetimes', 'https://example.com/rust/lifetimes', 0, 0, NULL, '2026-02-25 00:00:00', '2026-02-25 00:00:00');
            "#,
        ))
        .await;

        let response = list_json_index(&server, None).await;
        assert_eq!(response.items.len(), 1);
        assert_eq!(response.items[0].title, "Understanding Rust Lifetimes");
    }

    #[tokio::test]
    async fn index_json_preserves_non_site_dash_titles() {
        let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Rust - The Book', 'https://doc.rust-lang.org/book', 'https://doc.rust-lang.org/book', 0, 0, NULL, '2026-02-25 00:00:00', '2026-02-25 00:00:00');
            "#,
        ))
        .await;

        let response = list_json_index(&server, None).await;
        assert_eq!(response.items.len(), 1);
        assert_eq!(response.items[0].title, "Rust - The Book");
    }
}
