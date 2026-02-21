use std::{collections::HashMap, fmt::Display};

use axum::{
    Json, Router,
    body::{self, Body},
    extract::{Form, Path, Request, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing,
};
use sailfish::Template;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use serde::{Deserialize, Serialize};

use crate::{
    entity::{
        hyperlink,
        hyperlink_artifact::{self, HyperlinkArtifactKind},
        hyperlink_processing_job,
    },
    model::hyperlink::HyperlinkInput,
    model::hyperlink::ROOT_DISCOVERY_DEPTH,
    server::context::Context,
};

use super::views;

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

async fn index(State(state): State<Context>) -> Response {
    index_with_kind(&state, ResponseKind::Text).await
}

async fn new() -> Response {
    views::render_html_page("New Hyperlink", render_new())
}

async fn index_json(State(state): State<Context>) -> Response {
    index_with_kind(&state, ResponseKind::Json).await
}

async fn create(State(state): State<Context>, Form(input): Form<HyperlinkInput>) -> Response {
    create_with_kind(&state, input, ResponseKind::Text).await
}

async fn create_json(State(state): State<Context>, Json(input): Json<HyperlinkInput>) -> Response {
    create_with_kind(&state, input, ResponseKind::Json).await
}

async fn show_by_path(Path(id_or_ext): Path<String>, State(state): State<Context>) -> Response {
    let (id, kind) = match parse_id_and_kind(&id_or_ext) {
        Ok(parts) => parts,
        Err(err) => return err.into_response(),
    };
    show_with_kind(&state, id, kind).await
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
    update_with_kind(&state, id, input, kind).await
}

async fn update_html_post(
    Path(id): Path<i32>,
    State(state): State<Context>,
    Form(input): Form<HyperlinkInput>,
) -> Response {
    update_with_kind(&state, id, input, ResponseKind::Text).await
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
    delete_with_kind(&state, id, kind).await
}

async fn delete_html_post(Path(id): Path<i32>, State(state): State<Context>) -> Response {
    delete_with_kind(&state, id, ResponseKind::Text).await
}

async fn reprocess(Path(id): Path<i32>, State(state): State<Context>) -> Response {
    match crate::model::hyperlink::enqueue_reprocess_by_id(
        &state.connection,
        id,
        state.processing_queue.as_ref(),
    )
    .await
    {
        Ok(Some(link)) => Redirect::to(&show_path(link.id)).into_response(),
        Ok(None) => hyperlink_not_found(ResponseKind::Text, id),
        Err(err) => hyperlink_internal_error(ResponseKind::Text, id, "enqueue for processing", err),
    }
}

async fn download_latest_artifact(
    Path((id, kind)): Path<(i32, String)>,
    State(state): State<Context>,
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

    let filename = format!(
        "hyperlink-{id}-{}.{}",
        artifact_kind_slug(&kind),
        artifact_kind_file_extension(&kind)
    );
    let mut response = Response::new(Body::from(artifact.payload));
    *response.status_mut() = StatusCode::OK;

    let content_type = HeaderValue::from_str(&artifact.content_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, content_type);

    if let Ok(disposition) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
    {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, disposition);
    }

    response
}

async fn index_with_kind(state: &Context, kind: ResponseKind) -> Response {
    let links = match hyperlink::Entity::find()
        .filter(hyperlink::Column::DiscoveryDepth.eq(ROOT_DISCOVERY_DEPTH))
        .order_by_desc(hyperlink::Column::CreatedAt)
        .all(&state.connection)
        .await
    {
        Ok(links) => links,
        Err(err) => {
            return response_error(
                kind,
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to list hyperlinks: {err}"),
            );
        }
    };

    let hyperlink_ids = links.iter().map(|link| link.id).collect::<Vec<_>>();
    let latest_jobs = match crate::model::hyperlink_processing_job::latest_for_hyperlinks(
        &state.connection,
        &hyperlink_ids,
    )
    .await
    {
        Ok(latest_jobs) => latest_jobs,
        Err(err) => {
            return response_error(
                kind,
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load processing jobs: {err}"),
            );
        }
    };

    match kind {
        ResponseKind::Text => {
            views::render_html_page("Hyperlinks", render_index(&links, &latest_jobs))
        }
        ResponseKind::Json => {
            let response = links
                .iter()
                .map(|link| to_response(link, latest_jobs.get(&link.id)))
                .collect::<Vec<_>>();
            (StatusCode::OK, Json(response)).into_response()
        }
    }
}

async fn create_with_kind(state: &Context, input: HyperlinkInput, kind: ResponseKind) -> Response {
    let input = match crate::model::hyperlink::validate_and_normalize(input).await {
        Ok(input) => input,
        Err(msg) => return response_error(kind, StatusCode::BAD_REQUEST, msg),
    };

    match crate::model::hyperlink::insert(&state.connection, input, state.processing_queue.as_ref())
        .await
    {
        Ok(link) => {
            let latest_job = latest_job_optional(state, link.id).await;
            write_success_response(kind, StatusCode::CREATED, &link, latest_job.as_ref())
        }
        Err(err) => response_error(
            kind,
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to create hyperlink: {err}"),
        ),
    }
}

async fn show_with_kind(state: &Context, id: i32, kind: ResponseKind) -> Response {
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

            match kind {
                ResponseKind::Text => views::render_html_page(
                    "Show Hyperlink",
                    render_show(
                        &link,
                        latest_job.as_ref(),
                        &latest_artifacts,
                        &recent_jobs,
                        &discovered_links,
                        &discovered_latest_jobs,
                    ),
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

async fn edit(Path(id): Path<i32>, State(state): State<Context>) -> Response {
    match hyperlink::Entity::find_by_id(id)
        .one(&state.connection)
        .await
    {
        Ok(Some(link)) => views::render_html_page("Edit Hyperlink", render_edit(&link)),
        Ok(None) => hyperlink_not_found(ResponseKind::Text, id),
        Err(err) => hyperlink_internal_error(ResponseKind::Text, id, "fetch", err),
    }
}

async fn update_with_kind(
    state: &Context,
    id: i32,
    input: HyperlinkInput,
    kind: ResponseKind,
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
            write_success_response(kind, StatusCode::OK, &link, latest_job.as_ref())
        }
        Ok(None) => hyperlink_not_found(kind, id),
        Err(err) => hyperlink_internal_error(kind, id, "update", err),
    }
}

async fn delete_with_kind(state: &Context, id: i32, kind: ResponseKind) -> Response {
    match hyperlink::Entity::delete_by_id(id)
        .exec(&state.connection)
        .await
    {
        Ok(result) if result.rows_affected == 0 => hyperlink_not_found(kind, id),
        Ok(_) => match kind {
            ResponseKind::Text => Redirect::to(HYPERLINKS_PATH).into_response(),
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
) -> Response {
    match kind {
        ResponseKind::Text => Redirect::to(&show_path(link.id)).into_response(),
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
}

fn render_index(
    links: &[hyperlink::Model],
    latest_jobs: &HashMap<i32, hyperlink_processing_job::Model>,
) -> Result<String, sailfish::RenderError> {
    HyperlinksIndexTemplate { links, latest_jobs }.render()
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
}

impl<'a> HyperlinksShowTemplate<'a> {
    fn processing_state(&self) -> &'static str {
        processing_state_name(self.latest_job)
    }

    fn latest_job_kind(&self) -> Option<&'static str> {
        self.latest_job
            .map(|job| crate::model::hyperlink_processing_job::kind_name(job.kind.clone()))
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
}

fn render_show(
    link: &hyperlink::Model,
    latest_job: Option<&hyperlink_processing_job::Model>,
    latest_artifacts: &HashMap<HyperlinkArtifactKind, hyperlink_artifact::Model>,
    recent_jobs: &[hyperlink_processing_job::Model],
    discovered_links: &[hyperlink::Model],
    discovered_latest_jobs: &HashMap<i32, hyperlink_processing_job::Model>,
) -> Result<String, sailfish::RenderError> {
    HyperlinksShowTemplate {
        link,
        latest_job,
        latest_artifacts,
        recent_jobs,
        discovered_links,
        discovered_latest_jobs,
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

fn show_artifact_kinds() -> [HyperlinkArtifactKind; 6] {
    [
        HyperlinkArtifactKind::SnapshotWarc,
        HyperlinkArtifactKind::PdfSource,
        HyperlinkArtifactKind::SnapshotError,
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
        HyperlinkArtifactKind::ReadableText => ("readable_text", "Readable Markdown", "md", false),
        HyperlinkArtifactKind::ReadableMeta => {
            ("readable_meta", "Readable Metadata", "json", false)
        }
        HyperlinkArtifactKind::ReadableError => ("readable_error", "Readable Error", "json", true),
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
        test_support::initialize_hyperlinks_schema(&connection).await;
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

    async fn list_json_hyperlinks(server: &TestServer) -> Vec<HyperlinkResponse> {
        let list = server.get("/hyperlinks.json").await;
        list.assert_status_ok();
        list.json()
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
}
