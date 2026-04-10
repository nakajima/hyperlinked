use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing,
};
use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait};
use serde::Serialize;

use crate::{
    app::models::{
        artifact_job::{self, ArtifactFetchMode, ArtifactJobResolveResult},
        hyperlink_artifact as hyperlink_artifact_model, hyperlink_processing_job, settings,
        upload::{
            DEFAULT_FILENAME, find_existing_pdf_upload, latest_job_optional, looks_like_pdf,
            normalized_upload_title, now_utc, pending_upload_placeholder, sanitize_pdf_filename,
            sha256_hex, upload_filename_from_url, upload_hyperlink_url,
        },
    },
    entity::{
        hyperlink, hyperlink_artifact::HyperlinkArtifactKind,
        hyperlink_processing_job::HyperlinkProcessingJobKind,
    },
    server::context::Context,
};

#[cfg(test)]
pub(crate) use crate::app::models::upload::UPLOADS_PREFIX;

const MAX_UPLOAD_SIZE_BYTES: usize = 100 * 1024 * 1024;
const MULTIPART_BODY_LIMIT_BYTES: usize = MAX_UPLOAD_SIZE_BYTES + 1024 * 1024;
const PDF_CONTENT_TYPE: &str = "application/pdf";

pub fn routes() -> Router<Context> {
    Router::new()
        .route(
            "/uploads",
            routing::post(create_upload).layer(DefaultBodyLimit::max(MULTIPART_BODY_LIMIT_BYTES)),
        )
        .route("/uploads/{id}/{filename}", routing::get(download_upload))
}

#[derive(Clone, Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Clone, Debug, Serialize)]
struct HyperlinkResponse {
    id: i32,
    title: String,
    url: String,
    raw_url: String,
    summary: Option<String>,
    source_type: String,
    clicks_count: i32,
    last_clicked_at: Option<String>,
    processing_state: String,
    created_at: String,
    updated_at: String,
}

#[derive(Clone, Debug, Default)]
struct ParsedUpload {
    upload_type: Option<String>,
    title: Option<String>,
    filename_override: Option<String>,
    file_part_filename: Option<String>,
    file_payload: Option<Vec<u8>>,
}

async fn create_upload(State(state): State<Context>, mut multipart: Multipart) -> Response {
    let parsed = match parse_upload_multipart(&mut multipart).await {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };

    if parsed
        .upload_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        != Some("pdf")
    {
        return response_error(StatusCode::BAD_REQUEST, "upload_type must be `pdf`");
    }

    let payload = match parsed.file_payload {
        Some(payload) if !payload.is_empty() => payload,
        _ => return response_error(StatusCode::BAD_REQUEST, "file is required"),
    };

    if !looks_like_pdf(&payload) {
        return response_error(StatusCode::BAD_REQUEST, "uploaded file is not a PDF");
    }

    let requested_filename = parsed
        .filename_override
        .or(parsed.file_part_filename)
        .unwrap_or_else(|| DEFAULT_FILENAME.to_string());
    let filename = sanitize_pdf_filename(&requested_filename);
    let checksum = sha256_hex(&payload);

    if let Some(existing) =
        find_existing_pdf_upload(&state.connection, checksum.as_str(), filename.as_str()).await
    {
        let latest_job = latest_job_optional(&state.connection, existing.id).await;
        return (
            StatusCode::OK,
            Json(to_response(&existing, latest_job.as_ref())),
        )
            .into_response();
    }

    let title = normalized_upload_title(parsed.title.as_deref(), filename.as_str());
    let placeholder_url = pending_upload_placeholder(filename.as_str());

    let inserted = match (hyperlink::ActiveModel {
        title: Set(title),
        url: Set(placeholder_url.clone()),
        raw_url: Set(placeholder_url),
        discovery_depth: Set(crate::app::models::hyperlink::ROOT_DISCOVERY_DEPTH),
        clicks_count: Set(0),
        source_type: Set(hyperlink::HyperlinkSourceType::Pdf),
        created_at: Set(now_utc()),
        updated_at: Set(now_utc()),
        ..Default::default()
    })
    .insert(&state.connection)
    .await
    {
        Ok(model) => model,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to create upload hyperlink: {err}"),
            );
        }
    };

    let mut active: hyperlink::ActiveModel = inserted.into();
    let final_url = upload_hyperlink_url(*active.id.as_ref(), filename.as_str());
    active.url = Set(final_url.clone());
    active.raw_url = Set(final_url);
    active.updated_at = Set(now_utc());
    let hyperlink = match active.update(&state.connection).await {
        Ok(model) => model,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to finalize upload hyperlink url: {err}"),
            );
        }
    };

    if let Err(err) = hyperlink_artifact_model::insert(
        &state.connection,
        hyperlink.id,
        None,
        HyperlinkArtifactKind::PdfSource,
        payload,
        PDF_CONTENT_TYPE,
    )
    .await
    {
        return response_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to persist uploaded pdf artifact: {err}"),
        );
    }

    if let Err(err) = enqueue_upload_processing_jobs(&state, hyperlink.id).await {
        return response_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to enqueue upload processing jobs: {err}"),
        );
    }

    let latest_job = latest_job_optional(&state.connection, hyperlink.id).await;
    (
        StatusCode::CREATED,
        Json(to_response(&hyperlink, latest_job.as_ref())),
    )
        .into_response()
}

async fn download_upload(
    Path((id, filename)): Path<(i32, String)>,
    State(state): State<Context>,
) -> Response {
    let Some(link) = hyperlink::Entity::find_by_id(id)
        .one(&state.connection)
        .await
        .map_err(|err| {
            response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to fetch upload hyperlink: {err}"),
            )
        })
        .ok()
        .flatten()
    else {
        return response_error(StatusCode::NOT_FOUND, "upload not found");
    };

    let expected_filename = upload_filename_from_url(link.url.as_str());
    if expected_filename.as_deref() != Some(filename.as_str()) {
        return response_error(StatusCode::NOT_FOUND, "upload not found");
    }

    let artifact = match hyperlink_artifact_model::latest_for_hyperlink_kind(
        &state.connection,
        id,
        HyperlinkArtifactKind::PdfSource,
    )
    .await
    {
        Ok(Some(artifact)) => artifact,
        Ok(None) => return response_error(StatusCode::NOT_FOUND, "upload artifact not found"),
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load upload artifact: {err}"),
            );
        }
    };

    let payload = match hyperlink_artifact_model::load_payload(&artifact).await {
        Ok(payload) => payload,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to read upload artifact payload: {err}"),
            );
        }
    };

    let mut response = Response::new(Body::from(payload));
    *response.status_mut() = StatusCode::OK;
    let content_type = HeaderValue::from_str(&artifact.content_type)
        .unwrap_or_else(|_| HeaderValue::from_static(PDF_CONTENT_TYPE));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, content_type);
    response
}

async fn parse_upload_multipart(multipart: &mut Multipart) -> Result<ParsedUpload, Response> {
    let mut parsed = ParsedUpload::default();

    while let Some(mut field) = multipart.next_field().await.map_err(|err| {
        response_error(
            StatusCode::BAD_REQUEST,
            format!("failed to parse multipart upload: {err}"),
        )
    })? {
        let name = field.name().unwrap_or_default();
        match name {
            "upload_type" => {
                let value = field.text().await.map_err(|err| {
                    response_error(
                        StatusCode::BAD_REQUEST,
                        format!("failed to read upload_type field: {err}"),
                    )
                })?;
                parsed.upload_type = Some(value);
            }
            "title" => {
                let value = field.text().await.map_err(|err| {
                    response_error(
                        StatusCode::BAD_REQUEST,
                        format!("failed to read title field: {err}"),
                    )
                })?;
                parsed.title = Some(value);
            }
            "filename" => {
                let value = field.text().await.map_err(|err| {
                    response_error(
                        StatusCode::BAD_REQUEST,
                        format!("failed to read filename field: {err}"),
                    )
                })?;
                parsed.filename_override = Some(value);
            }
            "file" => {
                if parsed.file_payload.is_some() {
                    return Err(response_error(
                        StatusCode::BAD_REQUEST,
                        "only one file field is supported",
                    ));
                }

                parsed.file_part_filename = field.file_name().map(ToString::to_string);
                let mut payload = Vec::new();
                let mut total_size = 0usize;
                while let Some(chunk) = field.chunk().await.map_err(|err| {
                    response_error(
                        StatusCode::BAD_REQUEST,
                        format!("failed to read uploaded file chunk: {err}"),
                    )
                })? {
                    total_size = total_size.saturating_add(chunk.len());
                    if total_size > MAX_UPLOAD_SIZE_BYTES {
                        return Err(response_error(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            format!(
                                "uploaded file exceeds {} bytes limit",
                                MAX_UPLOAD_SIZE_BYTES
                            ),
                        ));
                    }
                    payload.extend_from_slice(&chunk);
                }
                parsed.file_payload = Some(payload);
            }
            _ => {}
        }
    }

    Ok(parsed)
}

async fn enqueue_upload_processing_jobs(state: &Context, hyperlink_id: i32) -> Result<(), String> {
    let Some(queue) = state.processing_queue.as_ref() else {
        return Ok(());
    };

    let collection_settings = settings::load(&state.connection)
        .await
        .map_err(|err| format!("failed to load artifact collection settings: {err}"))?;

    let result = artifact_job::resolve_and_enqueue_for_job_kind_with_settings(
        &state.connection,
        hyperlink_id,
        HyperlinkProcessingJobKind::Snapshot,
        ArtifactFetchMode::RefetchTarget,
        collection_settings,
        Some(queue),
    )
    .await
    .map_err(|err| format!("failed to resolve snapshot job dependencies: {err}"))?;
    if matches!(result, ArtifactJobResolveResult::UnsupportedJobKind { .. }) {
        return Err(
            "failed to enqueue snapshot job: unsupported snapshot artifact job kind".to_string(),
        );
    }

    Ok(())
}

fn response_error(status: StatusCode, message: impl Into<String>) -> Response {
    let payload = ErrorResponse {
        error: message.into(),
    };
    (status, Json(payload)).into_response()
}

fn to_response(
    link: &hyperlink::Model,
    latest_job: Option<&crate::entity::hyperlink_processing_job::Model>,
) -> HyperlinkResponse {
    HyperlinkResponse {
        id: link.id,
        title: link.title.clone(),
        url: link.url.clone(),
        raw_url: link.raw_url.clone(),
        summary: link.summary.clone(),
        source_type: match link.source_type {
            hyperlink::HyperlinkSourceType::Unknown => "unknown".to_string(),
            hyperlink::HyperlinkSourceType::Html => "html".to_string(),
            hyperlink::HyperlinkSourceType::Pdf => "pdf".to_string(),
        },
        clicks_count: link.clicks_count,
        last_clicked_at: link.last_clicked_at.map(|value| value.to_string()),
        processing_state: latest_job
            .map(|job| hyperlink_processing_job::state_name(job.state.clone()).to_string())
            .unwrap_or_else(|| "ready".to_string()),
        created_at: link.created_at.to_string(),
        updated_at: link.updated_at.to_string(),
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/app_controllers_uploads_controller.rs"]
mod tests;
