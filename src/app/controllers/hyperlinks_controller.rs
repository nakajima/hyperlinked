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
    app::models::{
        artifact_job::{self, ArtifactFetchMode, ArtifactJobResolveResult},
        hyperlink::HyperlinkInput,
        hyperlink_search_doc, hyperlink_title, settings,
    },
    entity::{
        hyperlink::{self, HyperlinkSourceType},
        hyperlink_artifact::{self, HyperlinkArtifactKind},
        hyperlink_processing_job,
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
        .route("/hyperlinks/{id_or_ext}", routing::get(show_by_path))
        .route("/hyperlinks/{id_or_ext}", routing::patch(update_by_path))
        .route("/hyperlinks/{id_or_ext}", routing::delete(delete_by_path))
}

pub fn routes() -> Router<Context> {
    let router = links();

    #[cfg(test)]
    let router = router.merge(super::hyperlink_artifacts_controller::routes());

    router
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

    let canonicalized = match crate::app::models::url_canonicalize::canonicalize_submitted_url(url)
    {
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

    match crate::app::models::hyperlink::find_by_url(
        &state.connection,
        &canonicalized.canonical_url,
    )
    .await
    {
        Ok(Some(link)) => {
            let status =
                if link.discovery_depth == crate::app::models::hyperlink::ROOT_DISCOVERY_DEPTH {
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
    match crate::app::models::hyperlink::enqueue_reprocess_by_id(
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

pub(crate) async fn delete_artifact_kind(
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

pub(crate) async fn fetch_artifact_kind(
    Path((id, kind)): Path<(i32, String)>,
    State(state): State<Context>,
    headers: HeaderMap,
) -> Response {
    let Some(artifact_kind) = parse_artifact_kind(&kind) else {
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

    let Some(queue) = state.processing_queue.as_ref() else {
        return redirect_with_flash(
            &headers,
            &show_path(id),
            FlashName::Alert,
            "Queue workers are unavailable in this environment.",
        );
    };

    let result = match artifact_job::resolve_and_enqueue_for_artifact_kind(
        &state.connection,
        id,
        artifact_kind.clone(),
        ArtifactFetchMode::RefetchTarget,
        Some(queue),
    )
    .await
    {
        Ok(result) => result,
        Err(err) => {
            return hyperlink_internal_error(
                ResponseKind::Text,
                id,
                "enqueue artifact fetch for",
                err,
            );
        }
    };

    match result {
        ArtifactJobResolveResult::EnqueuedRequested { .. } => redirect_with_flash(
            &headers,
            &show_path(id),
            FlashName::Notice,
            format!("Queued fetch for {}.", artifact_kind_label(&artifact_kind)),
        ),
        ArtifactJobResolveResult::EnqueuedDependency {
            dependency_kind, ..
        } => redirect_with_flash(
            &headers,
            &show_path(id),
            FlashName::Notice,
            format!(
                "Queued {} first to satisfy dependencies for {}.",
                artifact_fetch_dependency_label(&dependency_kind),
                artifact_kind_label(&artifact_kind)
            ),
        ),
        ArtifactJobResolveResult::DisabledRequested { .. } => redirect_with_flash(
            &headers,
            &show_path(id),
            FlashName::Alert,
            format!(
                "Fetching {} is disabled by artifact settings.",
                artifact_kind_label(&artifact_kind)
            ),
        ),
        ArtifactJobResolveResult::DisabledDependency {
            dependency_kind, ..
        } => redirect_with_flash(
            &headers,
            &show_path(id),
            FlashName::Alert,
            format!(
                "Cannot fetch {} because {} is disabled by artifact settings.",
                artifact_kind_label(&artifact_kind),
                artifact_fetch_dependency_label(&dependency_kind)
            ),
        ),
        ArtifactJobResolveResult::UnfetchableDependency {
            dependency_kind, ..
        } => redirect_with_flash(
            &headers,
            &show_path(id),
            FlashName::Alert,
            format!(
                "Cannot fetch {} because {} cannot be fetched from this hyperlink URL.",
                artifact_kind_label(&artifact_kind),
                artifact_fetch_dependency_label(&dependency_kind)
            ),
        ),
        ArtifactJobResolveResult::AlreadySatisfied { .. } => redirect_with_flash(
            &headers,
            &show_path(id),
            FlashName::Notice,
            format!(
                "{} is already available; no fetch was queued.",
                artifact_kind_label(&artifact_kind)
            ),
        ),
        ArtifactJobResolveResult::UnsupportedArtifactKind { .. } => response_error(
            ResponseKind::Text,
            StatusCode::BAD_REQUEST,
            "unsupported artifact fetch kind",
        ),
        ArtifactJobResolveResult::UnsupportedJobKind { .. } => response_error(
            ResponseKind::Text,
            StatusCode::BAD_REQUEST,
            "unsupported artifact fetch kind",
        ),
    }
}

pub(crate) async fn download_latest_artifact(
    Path((id, kind)): Path<(i32, String)>,
    State(state): State<Context>,
) -> Response {
    serve_latest_artifact(state, id, kind, true).await
}

pub(crate) async fn render_latest_artifact_inline(
    Path((id, kind)): Path<(i32, String)>,
    State(state): State<Context>,
) -> Response {
    serve_latest_artifact(state, id, kind, false).await
}

pub(crate) async fn render_pdf_source_preview(
    Path(id): Path<i32>,
    State(state): State<Context>,
) -> Response {
    match hyperlink::Entity::find_by_id(id)
        .one(&state.connection)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return hyperlink_not_found(ResponseKind::Text, id),
        Err(err) => return hyperlink_internal_error(ResponseKind::Text, id, "fetch", err),
    }

    match crate::app::models::hyperlink_artifact::latest_for_hyperlink_kind(
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

    let artifact = match crate::app::models::hyperlink_artifact::latest_for_hyperlink_kind(
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

    let payload = match crate::app::models::hyperlink_artifact::load_payload(&artifact).await {
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

    match kind {
        ResponseKind::Text => {
            views::render_html_page_with_flash("Hyperlinks", render_index(&results), flash)
        }
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
    let input = match crate::app::models::hyperlink::validate_and_normalize(input).await {
        Ok(input) => input,
        Err(msg) => return response_error(kind, StatusCode::BAD_REQUEST, msg),
    };

    match crate::app::models::hyperlink::insert(
        &state.connection,
        input,
        state.processing_queue.as_ref(),
    )
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
            let latest_job =
                match crate::app::models::hyperlink_processing_job::latest_for_hyperlink(
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
                match crate::app::models::hyperlink_artifact::latest_for_hyperlink_kinds(
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

            let recent_jobs =
                match crate::app::models::hyperlink_processing_job::recent_for_hyperlink(
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
                match crate::app::models::hyperlink_relation::children_for_parent(
                    &state.connection,
                    id,
                )
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
                match crate::app::models::hyperlink_processing_job::latest_for_hyperlinks(
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
                match crate::app::models::hyperlink_artifact::latest_for_hyperlinks_kind(
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
                match crate::app::models::hyperlink_artifact::latest_for_hyperlinks_kind(
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
            let artifact_settings = match settings::load(&state.connection).await {
                Ok(settings) => settings,
                Err(err) => {
                    return hyperlink_internal_error(kind, id, "load artifact settings for", err);
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
                            artifact_settings,
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
    match crate::app::models::hyperlink::increment_click_count_by_id(&state.connection, id).await {
        Ok(Some(link)) => Redirect::temporary(&link.url).into_response(),
        Ok(None) => hyperlink_not_found(ResponseKind::Text, id),
        Err(err) => hyperlink_internal_error(ResponseKind::Text, id, "visit", err),
    }
}

async fn click(Path(id): Path<i32>, State(state): State<Context>) -> Response {
    match crate::app::models::hyperlink::increment_click_count_by_id(&state.connection, id).await {
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
    let input = match crate::app::models::hyperlink::validate_and_normalize(input).await {
        Ok(input) => input,
        Err(msg) => return response_error(kind, StatusCode::BAD_REQUEST, msg),
    };

    match crate::app::models::hyperlink::update_by_id(
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
    match crate::app::models::hyperlink::delete_by_id_with_tombstone(&state.connection, id).await {
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
        Some(job) => crate::app::models::hyperlink_processing_job::state_name(job.state.clone()),
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
    crate::app::models::hyperlink_processing_job::latest_for_hyperlink(
        &state.connection,
        hyperlink_id,
    )
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
}

fn render_index(results: &HyperlinkFetchResults) -> Result<String, sailfish::RenderError> {
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
    artifact_settings: settings::ArtifactCollectionSettings,
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

    fn artifact_fetch_enabled(&self, kind: &HyperlinkArtifactKind) -> bool {
        artifact_job::artifact_kind_fetch_enabled(kind, self.artifact_settings)
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
    artifact_settings: settings::ArtifactCollectionSettings,
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
        artifact_settings,
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

fn artifact_fetch_dependency_label(
    kind: &hyperlink_processing_job::HyperlinkProcessingJobKind,
) -> &'static str {
    match kind {
        hyperlink_processing_job::HyperlinkProcessingJobKind::Snapshot => "source artifacts",
        hyperlink_processing_job::HyperlinkProcessingJobKind::Og => "Open Graph metadata",
        hyperlink_processing_job::HyperlinkProcessingJobKind::Readability => {
            "readability artifacts"
        }
        hyperlink_processing_job::HyperlinkProcessingJobKind::Oembed => "oEmbed metadata",
        hyperlink_processing_job::HyperlinkProcessingJobKind::SublinkDiscovery => {
            "sublink discovery"
        }
    }
}

fn artifact_kind_file_extension(kind: &HyperlinkArtifactKind) -> &'static str {
    artifact_kind_info(kind).2
}

fn artifact_download_file_extension(
    kind: &HyperlinkArtifactKind,
    artifact: &hyperlink_artifact::Model,
) -> String {
    if *kind == HyperlinkArtifactKind::SnapshotWarc
        && crate::app::models::hyperlink_artifact::is_snapshot_warc_gzip_artifact(artifact)
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
#[path = "../../../tests/unit/app_controllers_hyperlinks_controller.rs"]
mod tests;
