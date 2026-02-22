use std::{collections::HashMap, fmt::Display};

use axum::{
    Json, Router,
    body::{self, Body},
    extract::{Form, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing,
};
use sailfish::Template;
use sea_orm::EntityTrait;
use serde::{Deserialize, Serialize};

use crate::{
    entity::{
        hyperlink,
        hyperlink_artifact::{self, HyperlinkArtifactKind},
        hyperlink_processing_job,
    },
    model::hyperlink::HyperlinkInput,
    server::{
        context::Context,
        flash::{Flash, FlashName, redirect_with_flash},
    },
};

use super::{
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
        .route("/hyperlinks", routing::post(create))
        .route("/hyperlinks.json", routing::post(create_json))
        .route("/hyperlinks/{id}/click", routing::post(click))
        .route("/hyperlinks/{id}/visit", routing::get(visit))
        .route("/hyperlinks/{id}/edit", routing::get(edit))
        .route("/hyperlinks/{id}/update", routing::post(update_html_post))
        .route("/hyperlinks/{id}/delete", routing::post(delete_html_post))
        .route("/hyperlinks/{id}/reprocess", routing::post(reprocess))
        .route(
            "/hyperlinks/{id}/artifacts/{kind}",
            routing::get(download_latest_artifact),
        )
        .route(
            "/hyperlinks/{id}/artifacts/{kind}/inline",
            routing::get(render_latest_artifact_inline),
        )
        .route("/hyperlinks/{id_or_ext}", routing::get(show_by_path))
        .route("/hyperlinks/{id_or_ext}", routing::patch(update_by_path))
        .route("/hyperlinks/{id_or_ext}", routing::delete(delete_by_path))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HyperlinkResponse {
    id: i32,
    title: String,
    url: String,
    raw_url: String,
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
        Ok(Some(link)) => redirect_with_flash(
            &headers,
            &show_path(link.id),
            FlashName::Notice,
            "Queued reprocessing.",
        ),
        Ok(None) => hyperlink_not_found(ResponseKind::Text, id),
        Err(err) => hyperlink_internal_error(ResponseKind::Text, id, "enqueue for processing", err),
    }
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
        let filename = format!(
            "hyperlink-{id}-{}.{}",
            artifact_kind_slug(&kind),
            artifact_kind_file_extension(&kind)
        );
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
                    HyperlinkArtifactKind::ScreenshotThumbPng,
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
                    HyperlinkArtifactKind::ScreenshotThumbDarkPng,
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

            match kind {
                ResponseKind::Text => views::render_html_page_with_flash(
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
                    ),
                    flash,
                ),
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
    match hyperlink::Entity::delete_by_id(id)
        .exec(&state.connection)
        .await
    {
        Ok(result) if result.rows_affected == 0 => hyperlink_not_found(kind, id),
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
        title: model.title.clone(),
        url: model.url.clone(),
        raw_url: model.raw_url.clone(),
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

enum IndexStatus {
    Processing,
    Failed,
}

fn index_status(job: Option<&hyperlink_processing_job::Model>) -> Option<IndexStatus> {
    let job = job?;
    match job.state {
        hyperlink_processing_job::HyperlinkProcessingJobState::Queued
        | hyperlink_processing_job::HyperlinkProcessingJobState::Running => {
            Some(IndexStatus::Processing)
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
    thumbnail_artifacts: &'a HashMap<i32, hyperlink_artifact::Model>,
    dark_thumbnail_artifacts: &'a HashMap<i32, hyperlink_artifact::Model>,
    match_snippets: &'a HashMap<i32, String>,
    parsed_query: &'a crate::server::hyperlink_fetcher::ParsedHyperlinkQuery,
    query_input: &'a str,
    ignored_tokens: &'a [String],
    free_text: &'a str,
}

impl<'a> HyperlinksIndexTemplate<'a> {
    fn has_free_text(&self) -> bool {
        !self.free_text.trim().is_empty()
    }

    fn status_select_value(&self) -> &'static str {
        if self.parsed_query.statuses.is_empty()
            || self.parsed_query.statuses.contains(&StatusToken::All)
        {
            return "";
        }

        if self.parsed_query.statuses.len() != 1 {
            return "";
        }

        match self.parsed_query.statuses[0] {
            StatusToken::All => "",
            StatusToken::Processing => "processing",
            StatusToken::Failed => "failed",
            StatusToken::Idle => "idle",
            StatusToken::Succeeded => "succeeded",
        }
    }

    fn scope_select_value(&self) -> &'static str {
        let has_all = self.parsed_query.scopes.contains(&ScopeToken::All);
        let has_root = self.parsed_query.scopes.contains(&ScopeToken::Root);
        let has_discovered = self.parsed_query.scopes.contains(&ScopeToken::Discovered);

        if has_all || (has_root && has_discovered) {
            "all"
        } else if has_discovered {
            "discovered"
        } else {
            ""
        }
    }

    fn type_select_value(&self) -> &'static str {
        let has_all = self.parsed_query.types.contains(&TypeToken::All);
        let has_pdf = self.parsed_query.types.contains(&TypeToken::Pdf);
        let has_non_pdf = self.parsed_query.types.contains(&TypeToken::NonPdf);

        if has_all || (has_pdf && has_non_pdf) || self.parsed_query.types.is_empty() {
            ""
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

    fn thumbnail_inline_path(&self, hyperlink_id: i32) -> Option<String> {
        self.thumbnail_artifacts
            .contains_key(&hyperlink_id)
            .then_some(artifact_inline_path(
                hyperlink_id,
                &HyperlinkArtifactKind::ScreenshotThumbPng,
            ))
    }

    fn thumbnail_dark_inline_path(&self, hyperlink_id: i32) -> Option<String> {
        self.dark_thumbnail_artifacts
            .contains_key(&hyperlink_id)
            .then_some(artifact_inline_path(
                hyperlink_id,
                &HyperlinkArtifactKind::ScreenshotThumbDarkPng,
            ))
    }
}

fn render_index(results: &HyperlinkFetchResults) -> Result<String, sailfish::RenderError> {
    HyperlinksIndexTemplate {
        links: &results.links,
        latest_jobs: &results.latest_jobs,
        thumbnail_artifacts: &results.thumbnail_artifacts,
        dark_thumbnail_artifacts: &results.dark_thumbnail_artifacts,
        match_snippets: &results.match_snippets,
        parsed_query: &results.parsed_query,
        query_input: &results.raw_q,
        ignored_tokens: &results.ignored_tokens,
        free_text: &results.free_text,
    }
    .render()
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
}

impl<'a> HyperlinksShowTemplate<'a> {
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

    fn missing_artifact_labels(&self) -> Vec<&'static str> {
        required_show_artifact_kinds(&self.link.url, self.latest_artifacts)
            .into_iter()
            .filter(|kind| !self.latest_artifacts.contains_key(kind))
            .map(|kind| artifact_kind_label(&kind))
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

    fn screenshot_inline_path(&self) -> Option<String> {
        self.latest_artifacts
            .contains_key(&HyperlinkArtifactKind::ScreenshotPng)
            .then_some(self.artifact_inline_path(&HyperlinkArtifactKind::ScreenshotPng))
    }

    fn screenshot_dark_inline_path(&self) -> Option<String> {
        self.latest_artifacts
            .contains_key(&HyperlinkArtifactKind::ScreenshotDarkPng)
            .then_some(self.artifact_inline_path(&HyperlinkArtifactKind::ScreenshotDarkPng))
    }

    fn thumbnail_inline_path(&self, hyperlink_id: i32) -> Option<String> {
        if hyperlink_id == self.link.id
            && self
                .latest_artifacts
                .contains_key(&HyperlinkArtifactKind::ScreenshotThumbPng)
        {
            return Some(self.artifact_inline_path(&HyperlinkArtifactKind::ScreenshotThumbPng));
        }

        self.discovered_thumbnail_artifacts
            .contains_key(&hyperlink_id)
            .then_some(artifact_inline_path(
                hyperlink_id,
                &HyperlinkArtifactKind::ScreenshotThumbPng,
            ))
    }

    fn thumbnail_dark_inline_path(&self, hyperlink_id: i32) -> Option<String> {
        if hyperlink_id == self.link.id
            && self
                .latest_artifacts
                .contains_key(&HyperlinkArtifactKind::ScreenshotThumbDarkPng)
        {
            return Some(self.artifact_inline_path(&HyperlinkArtifactKind::ScreenshotThumbDarkPng));
        }

        self.discovered_dark_thumbnail_artifacts
            .contains_key(&hyperlink_id)
            .then_some(artifact_inline_path(
                hyperlink_id,
                &HyperlinkArtifactKind::ScreenshotThumbDarkPng,
            ))
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

fn show_artifact_kinds() -> [HyperlinkArtifactKind; 13] {
    [
        HyperlinkArtifactKind::SnapshotWarc,
        HyperlinkArtifactKind::PdfSource,
        HyperlinkArtifactKind::SnapshotError,
        HyperlinkArtifactKind::OembedMeta,
        HyperlinkArtifactKind::OembedError,
        HyperlinkArtifactKind::ScreenshotPng,
        HyperlinkArtifactKind::ScreenshotThumbPng,
        HyperlinkArtifactKind::ScreenshotDarkPng,
        HyperlinkArtifactKind::ScreenshotThumbDarkPng,
        HyperlinkArtifactKind::ScreenshotError,
        HyperlinkArtifactKind::ReadableText,
        HyperlinkArtifactKind::ReadableMeta,
        HyperlinkArtifactKind::ReadableError,
    ]
}

fn required_show_artifact_kinds(
    hyperlink_url: &str,
    latest_artifacts: &HashMap<HyperlinkArtifactKind, hyperlink_artifact::Model>,
) -> Vec<HyperlinkArtifactKind> {
    vec![
        required_source_artifact_kind(hyperlink_url, latest_artifacts),
        HyperlinkArtifactKind::ScreenshotPng,
        HyperlinkArtifactKind::ScreenshotDarkPng,
        HyperlinkArtifactKind::ScreenshotThumbPng,
        HyperlinkArtifactKind::ScreenshotThumbDarkPng,
        HyperlinkArtifactKind::OembedMeta,
        HyperlinkArtifactKind::ReadableText,
        HyperlinkArtifactKind::ReadableMeta,
    ]
}

fn required_source_artifact_kind(
    hyperlink_url: &str,
    latest_artifacts: &HashMap<HyperlinkArtifactKind, hyperlink_artifact::Model>,
) -> HyperlinkArtifactKind {
    if url_path_looks_pdf(hyperlink_url) {
        return HyperlinkArtifactKind::PdfSource;
    }

    if latest_artifacts.contains_key(&HyperlinkArtifactKind::SnapshotWarc) {
        return HyperlinkArtifactKind::SnapshotWarc;
    }

    if latest_artifacts.contains_key(&HyperlinkArtifactKind::PdfSource) {
        return HyperlinkArtifactKind::PdfSource;
    }

    HyperlinkArtifactKind::SnapshotWarc
}

fn url_path_looks_pdf(url: &str) -> bool {
    url.split(['?', '#'])
        .next()
        .is_some_and(|prefix| prefix.to_ascii_lowercase().ends_with(".pdf"))
}

fn artifact_kind_info(
    kind: &HyperlinkArtifactKind,
) -> (&'static str, &'static str, &'static str, bool) {
    match kind {
        HyperlinkArtifactKind::SnapshotWarc => ("snapshot_warc", "Snapshot WARC", "warc", false),
        HyperlinkArtifactKind::PdfSource => ("pdf_source", "PDF Source", "pdf", false),
        HyperlinkArtifactKind::SnapshotError => ("snapshot_error", "Snapshot Error", "json", true),
        HyperlinkArtifactKind::OembedMeta => ("oembed_meta", "oEmbed Metadata", "json", false),
        HyperlinkArtifactKind::OembedError => ("oembed_error", "oEmbed Error", "json", true),
        HyperlinkArtifactKind::ReadableText => ("readable_text", "Readable Markdown", "md", false),
        HyperlinkArtifactKind::ReadableMeta => {
            ("readable_meta", "Readable Metadata", "json", false)
        }
        HyperlinkArtifactKind::ReadableError => ("readable_error", "Readable Error", "json", true),
        HyperlinkArtifactKind::ScreenshotPng => ("screenshot_png", "Screenshot PNG", "png", false),
        HyperlinkArtifactKind::ScreenshotThumbPng => {
            ("screenshot_thumb_png", "Screenshot Thumbnail", "png", false)
        }
        HyperlinkArtifactKind::ScreenshotDarkPng => {
            ("screenshot_dark_png", "Screenshot Dark", "png", false)
        }
        HyperlinkArtifactKind::ScreenshotThumbDarkPng => (
            "screenshot_thumb_dark_png",
            "Screenshot Thumbnail Dark",
            "png",
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

fn artifact_kind_file_extension(kind: &HyperlinkArtifactKind) -> &'static str {
    artifact_kind_info(kind).2
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
    use serde::Serialize;
    use serde_json::json;

    async fn new_server() -> TestServer {
        new_server_with_seed(None).await
    }

    async fn new_server_with_seed(seed_sql: Option<&str>) -> TestServer {
        let connection = test_support::new_memory_connection().await;
        test_support::initialize_hyperlinks_schema_with_search(&connection).await;
        if let Some(seed_sql) = seed_sql {
            test_support::execute_sql(&connection, seed_sql).await;
        }

        let app = Router::<Context>::new().merge(links()).with_state(Context {
            connection,
            processing_queue: None,
        });
        TestServer::new(app).expect("test server should initialize")
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
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Paper', 'https://example.com/paper.pdf', 'https://example.com/paper.pdf', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
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
                    (4, 1, NULL, 'pdf_source', X'255044462D312E350A25', 'application/pdf', 10, '2026-02-19 00:00:03');
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

        server
            .get("/hyperlinks/1/artifacts/snapshot_warc")
            .await
            .assert_status_not_found();
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
}
