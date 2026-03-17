use std::{
    collections::{HashMap, HashSet},
    io::{ErrorKind, Read, Seek, Write},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Form, Multipart, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing,
};
use chrono::Utc;
use reqwest::header::{HeaderName, HeaderValue};
use sailfish::Template;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr,
    EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Statement,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::FileOptions};

use crate::{
    app::{
        helpers::admin_dashboard::{
            ADMIN_TAB_ARTIFACTS_PATH, ADMIN_TAB_IMPORT_EXPORT_PATH,
            ADMIN_TAB_LLM_INTERACTIONS_PATH, ADMIN_TAB_OVERVIEW_PATH, ADMIN_TAB_QUEUE_PATH,
            ADMIN_TAB_STORAGE_PATH, AdminTab, format_bytes, format_bytes_f64,
            format_relative_duration, llm_interactions_href,
        },
        models::{
            artifact_job::{self, ArtifactFetchMode},
            hyperlink_artifact as hyperlink_artifact_model,
            hyperlink_processing_job::{
                self as hyperlink_processing_job_model, ProcessingQueueSender,
            },
            hyperlink_search_doc,
            llm_discovery::{
                ChatApiKind, build_chat_request_body, chat_endpoint_candidates,
                extract_llm_model_ids, format_reqwest_transport_error,
                llm_backend_kind_for_chat_api, llm_backend_kind_for_models_path,
                llm_models_endpoints, truncate_model_discovery_body,
            },
            llm_interaction as llm_interaction_model,
            llm_settings::{self, LlmBackendKind, LlmProvider, LlmSettings},
            settings::{self, ArtifactCollectionSettings},
        },
    },
    entity::{
        hyperlink,
        hyperlink_artifact::{self, HyperlinkArtifactKind},
        hyperlink_processing_job::{self, HyperlinkProcessingJobKind, HyperlinkProcessingJobState},
        hyperlink_relation, llm_interaction,
    },
    integrations::mathpix::{MathpixStatus, MathpixUsageSummary},
    server::{
        admin_backup::{
            BackupCompletionSummary, BackupDownloadError, BackupProgress, BackupProgressStage,
            BackupStatusResponse,
        },
        admin_import::{
            ImportCompletionSummary, ImportProgress, ImportProgressStage, ImportStatusResponse,
            next_import_upload_path,
        },
        chromium_diagnostics::ChromiumDiagnostics,
        context::Context,
        flash::{Flash, FlashName, redirect_with_flash},
        font_diagnostics::FontDiagnostics,
    },
    storage::artifacts as artifact_storage,
};

use crate::server::{
    admin_jobs::{fetch_pending_queue_counts, set_all_queued_rows_cleared},
    views,
};

const BACKUP_VERSION: u32 = 1;
const BACKUP_MANIFEST_PATH: &str = "manifest.json";
const BACKUP_HYPERLINKS_PATH: &str = "hyperlinks.json";
const BACKUP_RELATIONS_PATH: &str = "relations.json";
const BACKUP_ARTIFACTS_PATH: &str = "artifacts.json";
const BACKUP_ARTIFACTS_DIR: &str = "artifacts";
const BACKUP_ARTIFACT_READ_CONCURRENCY: usize = 4;
const BACKUP_DEFLATE_LEVEL_BEST: i32 = 9;
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const LLM_MODELS_TIMEOUT: Duration = Duration::from_secs(10);
const LLM_INTERACTIONS_PER_PAGE: u64 = 50;
const DEFAULT_LLM_AUTH_HEADER_NAME: &str = "Authorization";
const DEFAULT_LLM_AUTH_HEADER_PREFIX: &str = "Bearer";

pub fn routes() -> Router<Context> {
    let router = Router::new()
        .route("/admin", routing::get(index))
        .route(ADMIN_TAB_OVERVIEW_PATH, routing::get(index_overview))
        .route(ADMIN_TAB_ARTIFACTS_PATH, routing::get(index_artifacts))
        .route(
            ADMIN_TAB_LLM_INTERACTIONS_PATH,
            routing::get(index_llm_interactions),
        )
        .route(ADMIN_TAB_QUEUE_PATH, routing::get(index_queue))
        .route(
            ADMIN_TAB_IMPORT_EXPORT_PATH,
            routing::get(index_import_export),
        )
        .route(ADMIN_TAB_STORAGE_PATH, routing::get(index_storage))
        .route("/admin/status", routing::get(status))
        .route(
            "/admin/artifact-settings",
            routing::post(update_artifact_settings),
        )
        .route(
            "/admin/process-missing-artifacts",
            routing::post(process_missing_artifacts),
        )
        .route("/admin/llm-settings", routing::post(update_llm_settings))
        .route(
            "/admin/clear-llm-interactions",
            routing::post(clear_llm_interactions),
        )
        .route("/admin/llm-models", routing::post(fetch_llm_models))
        .route("/admin/llm-check", routing::post(check_llm_endpoint));

    #[cfg(test)]
    let router = router
        .merge(super::backups_controller::routes())
        .merge(super::imports_controller::routes())
        .merge(super::queue_controller::routes());

    router
}

#[derive(Clone, Copy, Debug)]
struct LastRunSummary {
    snapshot_queued: usize,
    og_queued: usize,
    readability_queued: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct MissingArtifactsSummary {
    total_hyperlinks: usize,
    missing_source: usize,
    missing_og: usize,
    missing_readability: usize,
    snapshot_already_processing: usize,
    og_already_processing: usize,
    readability_already_processing: usize,
    snapshot_will_queue: usize,
    og_will_queue: usize,
    readability_will_queue: usize,
}

#[derive(Default)]
struct ArtifactPresence {
    has_source: bool,
    has_screenshot: bool,
    has_og_meta: bool,
    has_readable_text: bool,
    has_readable_meta: bool,
}

struct MissingArtifactsPlan {
    summary: MissingArtifactsSummary,
    snapshot_hyperlink_ids: Vec<i32>,
    screenshot_hyperlink_ids: Vec<i32>,
    og_hyperlink_ids: Vec<i32>,
    readability_hyperlink_ids: Vec<i32>,
}

#[derive(Debug, Deserialize)]
struct ArtifactSettingsForm {
    collect_source: Option<String>,
    collect_screenshots: Option<String>,
    collect_screenshot_dark: Option<String>,
    collect_og: Option<String>,
    collect_readability: Option<String>,
    delete_source_on_disable: Option<String>,
    delete_screenshots_on_disable: Option<String>,
    delete_og_on_disable: Option<String>,
    delete_readability_on_disable: Option<String>,
    queue_source_on_enable: Option<String>,
    queue_screenshots_on_enable: Option<String>,
    queue_og_on_enable: Option<String>,
    queue_readability_on_enable: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LlmSettingsForm {
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    auth_header_name: Option<String>,
    auth_header_prefix: Option<String>,
    backend_kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LlmModelsRequest {
    base_url: Option<String>,
    api_key: Option<String>,
    auth_header_name: Option<String>,
    auth_header_prefix: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LlmCheckRequest {
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    auth_header_name: Option<String>,
    auth_header_prefix: Option<String>,
    backend_kind: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct AdminLlmInteractionsQuery {
    page: Option<u64>,
}

#[derive(Clone, Debug, Default)]
struct AdminDatasetStats {
    total_hyperlinks: usize,
    root_hyperlinks: usize,
    discovered_hyperlinks: usize,
    total_artifacts: usize,
    total_processing_jobs: usize,
    active_processing_jobs: usize,
    db_size_total_bytes: u64,
    db_size_main_bytes: u64,
    db_size_wal_bytes: u64,
    db_size_shm_bytes: u64,
    saved_artifacts_size_bytes: i64,
    saved_artifacts_count: i64,
    discovered_artifacts_size_bytes: i64,
    discovered_artifacts_count: i64,
    artifact_storage_by_kind: Vec<ArtifactStorageByKind>,
}

#[derive(Clone, Debug, Default)]
struct AdminLlmInteractionsPage {
    items: Vec<llm_interaction::Model>,
    page: u64,
    total_pages: u64,
    total_items: u64,
    prev_page_href: Option<String>,
    next_page_href: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct ArtifactStorageByKind {
    kind: String,
    saved_size_bytes: i64,
    saved_artifact_count: i64,
    discovered_size_bytes: i64,
    discovered_artifact_count: i64,
}

#[derive(Clone, Debug, Default)]
struct ArtifactStorageBreakdown {
    by_kind: Vec<ArtifactStorageByKind>,
    saved_total_bytes: i64,
    saved_total_count: i64,
    discovered_total_bytes: i64,
    discovered_total_count: i64,
}

#[derive(Clone, Copy, Debug, Default)]
struct SqliteDiskStats {
    main_bytes: u64,
    wal_bytes: u64,
    shm_bytes: u64,
}

impl SqliteDiskStats {
    fn total_bytes(&self) -> u64 {
        self.main_bytes
            .saturating_add(self.wal_bytes)
            .saturating_add(self.shm_bytes)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct AdminImportSummary {
    hyperlinks: usize,
    relations: usize,
    artifacts: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BackupManifest {
    version: u32,
    exported_at: String,
    hyperlinks: usize,
    relations: usize,
    artifacts: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HyperlinkBackupRow {
    id: i32,
    title: String,
    url: String,
    raw_url: String,
    og_title: Option<String>,
    og_description: Option<String>,
    og_type: Option<String>,
    og_url: Option<String>,
    og_image_url: Option<String>,
    og_site_name: Option<String>,
    discovery_depth: i32,
    clicks_count: i32,
    last_clicked_at: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HyperlinkRelationBackupRow {
    id: i32,
    parent_hyperlink_id: i32,
    child_hyperlink_id: i32,
    created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HyperlinkArtifactBackupRow {
    id: i32,
    hyperlink_id: i32,
    kind: HyperlinkArtifactKind,
    content_type: String,
    size_bytes: i32,
    created_at: String,
    job_id: Option<i32>,
    checksum_sha256: Option<String>,
    payload_path: String,
}

#[derive(Clone, Debug)]
struct AdminBackupArchive {
    hyperlinks: Vec<HyperlinkBackupRow>,
    relations: Vec<HyperlinkRelationBackupRow>,
    artifacts: Vec<HyperlinkArtifactBackupRow>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AdminStatusResponse {
    queue: crate::server::admin_jobs::QueuePendingCounts,
    backup: BackupStatusResponse,
    import: ImportStatusResponse,
    server_time: String,
}

#[derive(Clone, Debug, Serialize)]
struct LlmModelsResponse {
    models: Vec<String>,
    backend_kind: String,
}

#[derive(Clone, Debug, Serialize)]
struct LlmCheckResponse {
    message: String,
    backend_kind: String,
}

#[derive(Clone, Debug, Serialize)]
struct AdminApiError {
    error: String,
}

#[derive(Clone, Debug)]
struct BuiltBackupZip {
    summary: BackupCompletionSummary,
}

async fn index(State(state): State<Context>, headers: HeaderMap) -> Response {
    render_admin_tab(AdminTab::Artifacts, &state, &headers, None).await
}

async fn index_overview(State(state): State<Context>, headers: HeaderMap) -> Response {
    render_admin_tab(AdminTab::Overview, &state, &headers, None).await
}

async fn index_artifacts(State(state): State<Context>, headers: HeaderMap) -> Response {
    render_admin_tab(AdminTab::Artifacts, &state, &headers, None).await
}

async fn index_llm_interactions(
    State(state): State<Context>,
    headers: HeaderMap,
    Query(query): Query<AdminLlmInteractionsQuery>,
) -> Response {
    render_admin_tab(AdminTab::LlmInteractions, &state, &headers, query.page).await
}

async fn index_queue(State(state): State<Context>, headers: HeaderMap) -> Response {
    render_admin_tab(AdminTab::Queue, &state, &headers, None).await
}

async fn index_import_export(State(state): State<Context>, headers: HeaderMap) -> Response {
    render_admin_tab(AdminTab::ImportExport, &state, &headers, None).await
}

async fn index_storage(State(state): State<Context>, headers: HeaderMap) -> Response {
    render_admin_tab(AdminTab::Storage, &state, &headers, None).await
}

async fn render_admin_tab(
    active_tab: AdminTab,
    state: &Context,
    headers: &HeaderMap,
    llm_interactions_page: Option<u64>,
) -> Response {
    let artifact_settings = match settings::load(&state.connection).await {
        Ok(settings) => settings,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load artifact settings: {err}"),
            );
        }
    };

    let plan = match build_missing_artifacts_plan(&state.connection, &artifact_settings).await {
        Ok(plan) => plan,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load missing-artifact summary: {err}"),
            );
        }
    };

    let stats = match build_dataset_stats(&state.connection).await {
        Ok(stats) => stats,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load admin stats: {err}"),
            );
        }
    };
    let llm_settings = match llm_settings::load(&state.connection).await {
        Ok(settings) => settings,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load llm settings: {err}"),
            );
        }
    };
    let llm_interactions = if active_tab == AdminTab::LlmInteractions {
        match build_llm_interactions_page(&state.connection, llm_interactions_page.unwrap_or(1))
            .await
        {
            Ok(page) => page,
            Err(err) => {
                return response_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to load llm interactions: {err}"),
                );
            }
        }
    } else {
        AdminLlmInteractionsPage::default()
    };
    let queue_pending = match fetch_pending_queue_counts(&state.connection).await {
        Ok(counts) => counts,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load queue pending counts: {err}"),
            );
        }
    };
    let chromium = crate::server::chromium_diagnostics::current();
    let fonts = crate::server::font_diagnostics::current();
    let mathpix = crate::integrations::mathpix::current_status();
    let mathpix_usage = crate::integrations::mathpix::current_usage_summary().await;

    views::render_html_page_with_admin_tabs_and_flash(
        "Admin",
        active_tab.path(),
        render_index(
            active_tab,
            &plan.summary,
            &stats,
            &artifact_settings,
            &llm_settings,
            llm_interactions,
            &queue_pending,
            &chromium,
            &fonts,
            &mathpix,
            &mathpix_usage,
        ),
        Flash::from_headers(headers),
    )
}

async fn status(State(state): State<Context>) -> Response {
    let queue = match fetch_pending_queue_counts(&state.connection).await {
        Ok(queue) => queue,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AdminApiError {
                    error: format!("failed to load queue status: {err}"),
                }),
            )
                .into_response();
        }
    };

    Json(AdminStatusResponse {
        queue,
        backup: state.backup_exports.snapshot(),
        import: state.backup_imports.snapshot(),
        server_time: format_datetime(&now_utc()),
    })
    .into_response()
}

pub(crate) async fn start_backup_export(State(state): State<Context>) -> Response {
    let backup_manager = state.backup_exports.clone();
    let backup_manager_for_job = backup_manager.clone();
    let connection = state.connection.clone();

    let started = backup_manager.start_job(move |job_id, output_path| {
        let backup_manager = backup_manager_for_job.clone();
        tokio::spawn(async move {
            let result = build_backup_zip(&connection, &output_path, |progress| {
                backup_manager.update_progress(job_id, progress)
            })
            .await;
            match result {
                Ok(archive) => {
                    backup_manager.mark_ready(job_id, archive.summary);
                }
                Err(message) => {
                    backup_manager.mark_failed(job_id, message);
                }
            }
        })
    });

    let status = if started.started {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    (status, Json(started.status)).into_response()
}

pub(crate) async fn cancel_backup_export(
    State(state): State<Context>,
    headers: HeaderMap,
) -> Response {
    let canceled = state.backup_exports.cancel_running();
    if canceled {
        return redirect_with_flash(
            &headers,
            ADMIN_TAB_IMPORT_EXPORT_PATH,
            FlashName::Notice,
            "Backup canceled.",
        );
    }
    redirect_with_flash(
        &headers,
        ADMIN_TAB_IMPORT_EXPORT_PATH,
        FlashName::Notice,
        "No backup in progress.",
    )
}

pub(crate) async fn download_backup_export(State(state): State<Context>) -> Response {
    match state.backup_exports.download_file_path() {
        Ok(path) => {
            let file = match tokio::fs::File::open(&path).await {
                Ok(file) => file,
                Err(error) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(AdminApiError {
                            error: format!("failed to open backup file for download: {error}"),
                        }),
                    )
                        .into_response();
                }
            };
            (
                [
                    (header::CONTENT_TYPE, "application/zip".to_string()),
                    (
                        header::CONTENT_DISPOSITION,
                        "attachment; filename=\"hyperlinked-backup.zip\"".to_string(),
                    ),
                ],
                Body::from_stream(ReaderStream::new(file)),
            )
                .into_response()
        }
        Err(BackupDownloadError::NotReady) => (
            StatusCode::CONFLICT,
            Json(AdminApiError {
                error: "backup is not ready yet".to_string(),
            }),
        )
            .into_response(),
        Err(BackupDownloadError::MissingPayload) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(AdminApiError {
                error: "backup payload is unavailable".to_string(),
            }),
        )
            .into_response(),
    }
}

pub(crate) async fn import_hyperlinks(
    State(state): State<Context>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let archive_path = match read_uploaded_backup_archive(&mut multipart).await {
        Ok(path) => path,
        Err(message) => {
            return redirect_with_flash(
                &headers,
                ADMIN_TAB_IMPORT_EXPORT_PATH,
                FlashName::Alert,
                format!("Import failed: {message}"),
            );
        }
    };

    let import_manager = state.backup_imports.clone();
    let import_manager_for_job = import_manager.clone();
    let connection = state.connection.clone();

    let started = import_manager.start_job(archive_path, move |job_id, archive_path| {
        let import_manager = import_manager_for_job.clone();
        tokio::spawn(async move {
            let result = import_backup_zip(&connection, &archive_path, |progress| {
                import_manager.update_progress(job_id, progress);
            })
            .await;

            match result {
                Ok(summary) => {
                    import_manager.mark_ready(
                        job_id,
                        ImportCompletionSummary {
                            hyperlinks: summary.hyperlinks,
                            relations: summary.relations,
                            artifacts: summary.artifacts,
                        },
                    );
                }
                Err(message) => {
                    import_manager.mark_failed(job_id, message);
                }
            }
        })
    });

    if started.started {
        redirect_with_flash(
            &headers,
            ADMIN_TAB_IMPORT_EXPORT_PATH,
            FlashName::Notice,
            "Import started. Refresh this page to follow progress.",
        )
    } else {
        let state_label = started.status.state;
        redirect_with_flash(
            &headers,
            ADMIN_TAB_IMPORT_EXPORT_PATH,
            FlashName::Notice,
            format!("Import already in progress ({state_label})."),
        )
    }
}

pub(crate) async fn cancel_backup_import(
    State(state): State<Context>,
    headers: HeaderMap,
) -> Response {
    let canceled = state.backup_imports.cancel_running();
    if canceled {
        return redirect_with_flash(
            &headers,
            ADMIN_TAB_IMPORT_EXPORT_PATH,
            FlashName::Notice,
            "Import canceled.",
        );
    }
    redirect_with_flash(
        &headers,
        ADMIN_TAB_IMPORT_EXPORT_PATH,
        FlashName::Notice,
        "No import in progress.",
    )
}

async fn process_missing_artifacts(State(state): State<Context>, headers: HeaderMap) -> Response {
    let artifact_settings = match settings::load(&state.connection).await {
        Ok(settings) => settings,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load artifact settings: {err}"),
            );
        }
    };

    let plan = match build_missing_artifacts_plan(&state.connection, &artifact_settings).await {
        Ok(plan) => plan,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to compute missing-artifact plan: {err}"),
            );
        }
    };

    let result = match execute_missing_artifacts_plan(
        &state.connection,
        state.processing_queue.as_ref(),
        &artifact_settings,
        plan,
    )
    .await
    {
        Ok(result) => result,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to enqueue missing-artifact jobs: {err}"),
            );
        }
    };

    redirect_with_flash(
        &headers,
        ADMIN_TAB_ARTIFACTS_PATH,
        FlashName::Notice,
        format!(
            "Queued {} snapshot job(s), {} og job(s), and {} readability job(s).",
            result.snapshot_queued, result.og_queued, result.readability_queued
        ),
    )
}

async fn update_artifact_settings(
    State(state): State<Context>,
    headers: HeaderMap,
    Form(form): Form<ArtifactSettingsForm>,
) -> Response {
    let current_settings = match settings::load(&state.connection).await {
        Ok(settings) => settings,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load current artifact settings: {err}"),
            );
        }
    };

    let requested_settings = ArtifactCollectionSettings {
        collect_source: checkbox_checked(&form.collect_source),
        collect_screenshots: checkbox_checked(&form.collect_screenshots),
        collect_screenshot_dark: checkbox_checked(&form.collect_screenshot_dark),
        collect_og: checkbox_checked(&form.collect_og),
        collect_readability: checkbox_checked(&form.collect_readability),
    };

    let updated_settings = match settings::save(&state.connection, requested_settings).await {
        Ok(settings) => settings,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to save artifact settings: {err}"),
            );
        }
    };

    let source_disabled = current_settings.collect_source && !updated_settings.collect_source;
    let source_enabled = !current_settings.collect_source && updated_settings.collect_source;
    let screenshots_disabled =
        current_settings.collect_screenshots && !updated_settings.collect_screenshots;
    let screenshots_enabled =
        !current_settings.collect_screenshots && updated_settings.collect_screenshots;
    let og_disabled = current_settings.collect_og && !updated_settings.collect_og;
    let og_enabled = !current_settings.collect_og && updated_settings.collect_og;
    let readability_disabled =
        current_settings.collect_readability && !updated_settings.collect_readability;
    let readability_enabled =
        !current_settings.collect_readability && updated_settings.collect_readability;

    let mut deleted_source = 0u64;
    let mut deleted_screenshots = 0u64;
    let mut deleted_og = 0u64;
    let mut deleted_readability = 0u64;

    if source_disabled && checkbox_checked(&form.delete_source_on_disable) {
        deleted_source = match delete_source_artifacts(&state.connection).await {
            Ok(count) => count,
            Err(err) => {
                return response_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to delete source artifacts: {err}"),
                );
            }
        };
    }

    if screenshots_disabled && checkbox_checked(&form.delete_screenshots_on_disable) {
        deleted_screenshots = match delete_screenshot_artifacts(&state.connection).await {
            Ok(count) => count,
            Err(err) => {
                return response_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to delete screenshot artifacts: {err}"),
                );
            }
        };
    }

    if og_disabled && checkbox_checked(&form.delete_og_on_disable) {
        deleted_og = match delete_og_artifacts_and_clear_fields(&state.connection).await {
            Ok(count) => count,
            Err(err) => {
                return response_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to delete open graph artifacts: {err}"),
                );
            }
        };
    }

    if readability_disabled && checkbox_checked(&form.delete_readability_on_disable) {
        deleted_readability =
            match delete_readability_artifacts_and_clear_search(&state.connection).await {
                Ok(count) => count,
                Err(err) => {
                    return response_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("failed to delete readability artifacts: {err}"),
                    );
                }
            };
    }

    let queue_source = source_enabled && checkbox_checked(&form.queue_source_on_enable);
    let queue_screenshots =
        screenshots_enabled && checkbox_checked(&form.queue_screenshots_on_enable);
    let queue_og = og_enabled && checkbox_checked(&form.queue_og_on_enable);
    let queue_readability =
        readability_enabled && checkbox_checked(&form.queue_readability_on_enable);
    let should_queue_backfill = queue_source || queue_screenshots || queue_og || queue_readability;

    let mut queued_snapshot = 0usize;
    let mut queued_screenshots = 0usize;
    let mut queued_og = 0usize;
    let mut queued_readability = 0usize;
    let mut queue_warning = None::<String>;

    if should_queue_backfill {
        let Some(queue) = state.processing_queue.as_ref() else {
            queue_warning = Some(
                "Backfill was requested, but queue workers are unavailable in this environment."
                    .to_string(),
            );
            return redirect_with_flash(
                &headers,
                ADMIN_TAB_ARTIFACTS_PATH,
                FlashName::Notice,
                build_artifact_settings_message(
                    deleted_source,
                    deleted_screenshots,
                    deleted_og,
                    deleted_readability,
                    queued_snapshot,
                    queued_screenshots,
                    queued_og,
                    queued_readability,
                    queue_warning.as_deref(),
                ),
            );
        };

        if queue_source || queue_og || queue_readability {
            let plan =
                match build_missing_artifacts_plan(&state.connection, &updated_settings).await {
                    Ok(plan) => plan,
                    Err(err) => {
                        return response_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("failed to build missing-artifact backfill plan: {err}"),
                        );
                    }
                };

            if queue_source {
                queued_snapshot = match enqueue_hyperlink_jobs(
                    &state.connection,
                    Some(queue),
                    HyperlinkProcessingJobKind::Snapshot,
                    &plan.snapshot_hyperlink_ids,
                )
                .await
                {
                    Ok(count) => count,
                    Err(err) => {
                        return response_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("failed to queue source backfill jobs: {err}"),
                        );
                    }
                };
            }

            if queue_og {
                queued_og = match enqueue_hyperlink_jobs(
                    &state.connection,
                    Some(queue),
                    HyperlinkProcessingJobKind::Og,
                    &plan.og_hyperlink_ids,
                )
                .await
                {
                    Ok(count) => count,
                    Err(err) => {
                        return response_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("failed to queue open graph backfill jobs: {err}"),
                        );
                    }
                };
            }

            if queue_readability {
                queued_readability = match enqueue_hyperlink_jobs(
                    &state.connection,
                    Some(queue),
                    HyperlinkProcessingJobKind::Readability,
                    &plan.readability_hyperlink_ids,
                )
                .await
                {
                    Ok(count) => count,
                    Err(err) => {
                        return response_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("failed to queue readability backfill jobs: {err}"),
                        );
                    }
                };
            }
        }

        if queue_screenshots {
            let missing_screenshot_hyperlink_ids =
                match build_missing_screenshot_hyperlink_ids(&state.connection).await {
                    Ok(ids) => ids,
                    Err(err) => {
                        return response_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("failed to build screenshot backfill plan: {err}"),
                        );
                    }
                };

            queued_screenshots = match enqueue_hyperlink_jobs(
                &state.connection,
                Some(queue),
                HyperlinkProcessingJobKind::Snapshot,
                &missing_screenshot_hyperlink_ids,
            )
            .await
            {
                Ok(count) => count,
                Err(err) => {
                    return response_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("failed to queue screenshot backfill jobs: {err}"),
                    );
                }
            };
        }
    }

    redirect_with_flash(
        &headers,
        ADMIN_TAB_ARTIFACTS_PATH,
        FlashName::Notice,
        build_artifact_settings_message(
            deleted_source,
            deleted_screenshots,
            deleted_og,
            deleted_readability,
            queued_snapshot,
            queued_screenshots,
            queued_og,
            queued_readability,
            queue_warning.as_deref(),
        ),
    )
}

async fn update_llm_settings(
    State(state): State<Context>,
    headers: HeaderMap,
    Form(form): Form<LlmSettingsForm>,
) -> Response {
    let current = match llm_settings::load(&state.connection).await {
        Ok(settings) => settings,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load current llm settings: {err}"),
            );
        }
    };

    let settings = LlmSettings {
        provider: LlmProvider::OpenAiCompatible,
        base_url: form.base_url.unwrap_or_default(),
        api_key: match form.api_key {
            Some(value) if value.trim().is_empty() => current.api_key,
            other => other,
        },
        model: form.model.unwrap_or_default(),
        auth_header_name: form.auth_header_name,
        auth_header_prefix: form.auth_header_prefix,
        backend_kind: match form.backend_kind {
            Some(value) => LlmBackendKind::from_storage(Some(value.as_str())),
            None => current.backend_kind,
        },
    };

    if let Some(api_key) = settings.api_key.as_deref() {
        let header_name = settings
            .auth_header_name
            .as_deref()
            .unwrap_or(DEFAULT_LLM_AUTH_HEADER_NAME);
        let header_prefix = settings
            .auth_header_prefix
            .as_deref()
            .unwrap_or(DEFAULT_LLM_AUTH_HEADER_PREFIX);
        let header_value = if header_prefix.trim().is_empty() {
            api_key.to_string()
        } else {
            format!("{header_prefix} {api_key}")
        };
        if let Err(error) = HeaderName::from_bytes(header_name.as_bytes()) {
            return redirect_with_flash(
                &headers,
                ADMIN_TAB_LLM_INTERACTIONS_PATH,
                FlashName::Alert,
                format!("Invalid auth header name: {error}"),
            );
        }
        if let Err(error) = HeaderValue::from_str(&header_value) {
            return redirect_with_flash(
                &headers,
                ADMIN_TAB_LLM_INTERACTIONS_PATH,
                FlashName::Alert,
                format!("Invalid auth header value: {error}"),
            );
        }
    }

    let saved = match llm_settings::save(&state.connection, settings).await {
        Ok(saved) => saved,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to save llm settings: {err}"),
            );
        }
    };

    let message = if saved.api_key.is_some() {
        "Updated LLM settings."
    } else {
        "Updated LLM settings. No API key is stored."
    };

    redirect_with_flash(
        &headers,
        ADMIN_TAB_LLM_INTERACTIONS_PATH,
        FlashName::Notice,
        message,
    )
}

async fn clear_llm_interactions(State(state): State<Context>, headers: HeaderMap) -> Response {
    let cleared = match llm_interaction_model::clear_all(&state.connection).await {
        Ok(cleared) => cleared,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to clear llm interactions: {err}"),
            );
        }
    };

    redirect_with_flash(
        &headers,
        ADMIN_TAB_LLM_INTERACTIONS_PATH,
        FlashName::Notice,
        format!("Cleared {cleared} LLM interaction(s)."),
    )
}

async fn fetch_llm_models(
    State(state): State<Context>,
    Json(request): Json<LlmModelsRequest>,
) -> Response {
    let current = match llm_settings::load(&state.connection).await {
        Ok(settings) => settings,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AdminApiError {
                    error: format!("failed to load current llm settings: {error}"),
                }),
            )
                .into_response();
        }
    };
    let LlmSettings {
        api_key: current_api_key,
        auth_header_name: current_auth_header_name,
        auth_header_prefix: current_auth_header_prefix,
        ..
    } = current;
    let LlmModelsRequest {
        base_url,
        api_key,
        auth_header_name,
        auth_header_prefix,
    } = request;

    let base_url = base_url.unwrap_or_default();
    let endpoints = match llm_models_endpoints(&base_url) {
        Ok(endpoints) => endpoints,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(AdminApiError {
                    error: format!("invalid base URL: {error}"),
                }),
            )
                .into_response();
        }
    };

    let client = match reqwest::Client::builder()
        .timeout(LLM_MODELS_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AdminApiError {
                    error: format!("failed to build model discovery client: {error}"),
                }),
            )
                .into_response();
        }
    };

    let api_key = trimmed_non_empty(api_key.as_deref()).or(current_api_key);
    let mut auth_header: Option<(HeaderName, HeaderValue)> = None;
    if let Some(api_key) = api_key {
        let header_name = match auth_header_name {
            Some(value) => trimmed_non_empty(Some(value.as_str())),
            None => current_auth_header_name,
        }
        .unwrap_or_else(|| DEFAULT_LLM_AUTH_HEADER_NAME.to_string());
        let header_prefix = match auth_header_prefix {
            Some(value) => trimmed_non_empty(Some(value.as_str())),
            None => current_auth_header_prefix,
        }
        .unwrap_or_else(|| DEFAULT_LLM_AUTH_HEADER_PREFIX.to_string());
        let header_value = if header_prefix.is_empty() {
            api_key
        } else {
            format!("{header_prefix} {api_key}")
        };

        let header_name = match HeaderName::from_bytes(header_name.as_bytes()) {
            Ok(header_name) => header_name,
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(AdminApiError {
                        error: format!("invalid auth header name: {error}"),
                    }),
                )
                    .into_response();
            }
        };
        let header_value = match HeaderValue::from_str(&header_value) {
            Ok(header_value) => header_value,
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(AdminApiError {
                        error: format!("invalid auth header value: {error}"),
                    }),
                )
                    .into_response();
            }
        };

        auth_header = Some((header_name, header_value));
    }

    let mut attempt_failures = Vec::new();

    for endpoint in endpoints {
        let detected_backend = llm_backend_kind_for_models_path(endpoint.path());
        let endpoint_display = endpoint.to_string();
        let mut builder = client.get(endpoint);
        if let Some((header_name, header_value)) = auth_header.as_ref() {
            builder = builder.header(header_name.clone(), header_value.clone());
        }

        let response = match builder.send().await {
            Ok(response) => response,
            Err(error) => {
                attempt_failures.push(format!(
                    "{endpoint_display} -> request failed: {}",
                    format_reqwest_transport_error(&error)
                ));
                continue;
            }
        };
        let status = response.status();
        let body = match response.text().await {
            Ok(body) => body,
            Err(error) => {
                attempt_failures.push(format!(
                    "{endpoint_display} -> failed to read response body: {error}"
                ));
                continue;
            }
        };

        if !status.is_success() {
            attempt_failures.push(format!(
                "{endpoint_display} -> status {status}: {}",
                truncate_model_discovery_body(&body)
            ));
            continue;
        }

        let payload: serde_json::Value = match serde_json::from_str(&body) {
            Ok(payload) => payload,
            Err(error) => {
                attempt_failures.push(format!(
                    "{endpoint_display} -> invalid json response: {error}"
                ));
                continue;
            }
        };

        return Json(LlmModelsResponse {
            models: extract_llm_model_ids(&payload),
            backend_kind: detected_backend.as_storage().to_string(),
        })
        .into_response();
    }

    (
        StatusCode::BAD_GATEWAY,
        Json(AdminApiError {
            error: format!(
                "model list discovery failed for base URL `{base_url}`. attempts: {}",
                attempt_failures.join(" | ")
            ),
        }),
    )
        .into_response()
}

async fn check_llm_endpoint(
    State(state): State<Context>,
    Json(request): Json<LlmCheckRequest>,
) -> Response {
    let current = match llm_settings::load(&state.connection).await {
        Ok(settings) => settings,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AdminApiError {
                    error: format!("failed to load current llm settings: {error}"),
                }),
            )
                .into_response();
        }
    };

    let base_url =
        trimmed_non_empty(request.base_url.as_deref()).unwrap_or_else(|| current.base_url.clone());
    if base_url.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(AdminApiError {
                error: "base URL is required".to_string(),
            }),
        )
            .into_response();
    }

    let model = trimmed_non_empty(request.model.as_deref()).unwrap_or(current.model.clone());
    if model.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(AdminApiError {
                error: "model is required".to_string(),
            }),
        )
            .into_response();
    }

    let backend_kind = match request.backend_kind.as_deref() {
        Some(value) => LlmBackendKind::from_storage(Some(value)),
        None => current.backend_kind,
    };
    let endpoints = match chat_endpoint_candidates(&base_url, backend_kind) {
        Ok(endpoints) => endpoints,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(AdminApiError { error })).into_response();
        }
    };

    let client = match reqwest::Client::builder()
        .timeout(LLM_MODELS_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AdminApiError {
                    error: format!("failed to build check client: {error}"),
                }),
            )
                .into_response();
        }
    };

    let provider = current.provider.as_storage().to_string();
    let api_key = trimmed_non_empty(request.api_key.as_deref()).or(current.api_key);
    let header_name = match request.auth_header_name {
        Some(value) => trimmed_non_empty(Some(value.as_str())),
        None => current.auth_header_name,
    }
    .unwrap_or_else(|| DEFAULT_LLM_AUTH_HEADER_NAME.to_string());
    let header_prefix = match request.auth_header_prefix {
        Some(value) => trimmed_non_empty(Some(value.as_str())),
        None => current.auth_header_prefix,
    }
    .unwrap_or_else(|| DEFAULT_LLM_AUTH_HEADER_PREFIX.to_string());

    let mut auth_header: Option<(HeaderName, HeaderValue)> = None;
    if let Some(api_key) = api_key {
        let header_value = if header_prefix.is_empty() {
            api_key
        } else {
            format!("{header_prefix} {api_key}")
        };
        let header_name = match HeaderName::from_bytes(header_name.as_bytes()) {
            Ok(name) => name,
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(AdminApiError {
                        error: format!("invalid auth header name: {error}"),
                    }),
                )
                    .into_response();
            }
        };
        let header_value = match HeaderValue::from_str(&header_value) {
            Ok(value) => value,
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(AdminApiError {
                        error: format!("invalid auth header value: {error}"),
                    }),
                )
                    .into_response();
            }
        };
        auth_header = Some((header_name, header_value));
    }

    let system_prompt = "Reply with strict JSON only.";
    let user_prompt = "{\"ok\":true}";
    let mut attempt_failures = Vec::new();
    for endpoint in endpoints {
        let endpoint_display = endpoint.url.to_string();
        let body = build_chat_request_body(endpoint.api_kind, &model, system_prompt, user_prompt);
        let request_body = llm_interaction_model::format_request_body(&body);
        let started = Instant::now();
        let mut builder = client
            .post(endpoint.url)
            .header(header::CONTENT_TYPE, "application/json")
            .json(&body);
        if let Some((header_name, header_value)) = auth_header.as_ref() {
            builder = builder.header(header_name.clone(), header_value.clone());
        }

        let response = match builder.send().await {
            Ok(response) => response,
            Err(error) => {
                let error_message = format_reqwest_transport_error(&error);
                record_llm_check_interaction(
                    &state.connection,
                    &provider,
                    &model,
                    &endpoint_display,
                    endpoint.api_kind,
                    &request_body,
                    None,
                    None,
                    Some(error_message.clone()),
                    started.elapsed(),
                )
                .await;
                attempt_failures.push(format!(
                    "{} [{}] -> request failed: {}",
                    endpoint_display,
                    endpoint.api_kind.as_str(),
                    error_message
                ));
                continue;
            }
        };
        let status = response.status();
        let body_text = match response.text().await {
            Ok(body) => body,
            Err(error) => {
                let error_message = format!("failed to read response body: {error}");
                record_llm_check_interaction(
                    &state.connection,
                    &provider,
                    &model,
                    &endpoint_display,
                    endpoint.api_kind,
                    &request_body,
                    None,
                    Some(status),
                    Some(error_message.clone()),
                    started.elapsed(),
                )
                .await;
                attempt_failures.push(format!(
                    "{} [{}] -> failed to read response body: {error}",
                    endpoint_display,
                    endpoint.api_kind.as_str()
                ));
                continue;
            }
        };

        if !status.is_success() {
            let error_message = format!(
                "status {status}: {}",
                truncate_model_discovery_body(&body_text)
            );
            record_llm_check_interaction(
                &state.connection,
                &provider,
                &model,
                &endpoint_display,
                endpoint.api_kind,
                &request_body,
                Some(body_text.clone()),
                Some(status),
                Some(error_message.clone()),
                started.elapsed(),
            )
            .await;
            attempt_failures.push(format!(
                "{} [{}] -> status {status}: {}",
                endpoint_display,
                endpoint.api_kind.as_str(),
                truncate_model_discovery_body(&body_text)
            ));
            continue;
        }

        let detected_backend = llm_backend_kind_for_chat_api(endpoint.api_kind);
        record_llm_check_interaction(
            &state.connection,
            &provider,
            &model,
            &endpoint_display,
            endpoint.api_kind,
            &request_body,
            Some(body_text),
            Some(status),
            None,
            started.elapsed(),
        )
        .await;
        return (
            StatusCode::OK,
            Json(LlmCheckResponse {
                message: format!(
                    "Check succeeded via {} [{}].",
                    endpoint_display,
                    endpoint.api_kind.as_str()
                ),
                backend_kind: detected_backend.as_storage().to_string(),
            }),
        )
            .into_response();
    }

    (
        StatusCode::BAD_GATEWAY,
        Json(AdminApiError {
            error: format!(
                "llm request failed across all endpoint candidates: {}",
                attempt_failures.join(" | ")
            ),
        }),
    )
        .into_response()
}

async fn record_llm_check_interaction(
    connection: &DatabaseConnection,
    provider: &str,
    model: &str,
    endpoint_url: &str,
    api_kind: ChatApiKind,
    request_body: &str,
    response_body: Option<String>,
    response_status: Option<reqwest::StatusCode>,
    error_message: Option<String>,
    duration: Duration,
) {
    if let Err(err) = llm_interaction_model::record(
        connection,
        llm_interaction_model::NewLlmInteraction {
            kind: "llm_check".to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            endpoint_url: endpoint_url.to_string(),
            api_kind: api_kind.as_str().to_string(),
            hyperlink_id: None,
            processing_job_id: None,
            admin_job_kind: None,
            admin_job_id: None,
            request_body: request_body.to_string(),
            response_body,
            response_status: response_status.map(|status| i32::from(status.as_u16())),
            error_message,
            duration_ms: Some(llm_interaction_model::duration_ms(duration)),
            created_at: None,
        },
    )
    .await
    {
        tracing::warn!(error = %err, "failed to record llm interaction");
    }
}

pub(crate) async fn clear_queue(State(state): State<Context>, headers: HeaderMap) -> Response {
    let cleared = match set_all_queued_rows_cleared(&state.connection).await {
        Ok(cleared) => cleared,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to clear queued queue rows: {err}"),
            );
        }
    };

    redirect_with_flash(
        &headers,
        ADMIN_TAB_QUEUE_PATH,
        FlashName::Notice,
        format!("Cleared {cleared} queued queue row(s)."),
    )
}

pub(crate) async fn pause_queue(State(state): State<Context>, headers: HeaderMap) -> Response {
    let Some(queue) = state.processing_queue.as_ref() else {
        return redirect_with_flash(
            &headers,
            ADMIN_TAB_QUEUE_PATH,
            FlashName::Alert,
            "Queue controls are unavailable in this environment.",
        );
    };

    if let Err(err) = queue.shutdown_worker().await {
        return response_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to pause queue workers: {err}"),
        );
    }

    redirect_with_flash(
        &headers,
        ADMIN_TAB_QUEUE_PATH,
        FlashName::Notice,
        "Queue workers paused.",
    )
}

pub(crate) async fn resume_queue(State(state): State<Context>, headers: HeaderMap) -> Response {
    let Some(queue) = state.processing_queue.as_ref() else {
        return redirect_with_flash(
            &headers,
            ADMIN_TAB_QUEUE_PATH,
            FlashName::Alert,
            "Queue controls are unavailable in this environment.",
        );
    };

    if let Err(err) = queue.spawn_worker(state.connection.clone()).await {
        return response_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to resume queue workers: {err}"),
        );
    }

    redirect_with_flash(
        &headers,
        ADMIN_TAB_QUEUE_PATH,
        FlashName::Notice,
        "Queue workers resumed.",
    )
}

async fn build_backup_zip<F>(
    connection: &DatabaseConnection,
    output_path: &Path,
    mut report_progress: F,
) -> Result<BuiltBackupZip, String>
where
    F: FnMut(BackupProgress),
{
    let build_result = async {
        report_progress(BackupProgress {
            stage: BackupProgressStage::LoadingRecords,
            artifacts_done: 0,
            artifacts_total: 0,
        });

        let hyperlinks = hyperlink::Entity::find()
            .order_by_asc(hyperlink::Column::Id)
            .all(connection)
            .await
            .map_err(|err| format!("failed to load hyperlinks: {err}"))?;
        let relations = hyperlink_relation::Entity::find()
            .order_by_asc(hyperlink_relation::Column::Id)
            .all(connection)
            .await
            .map_err(|err| format!("failed to load hyperlink relations: {err}"))?;
        let artifacts = hyperlink_artifact::Entity::find()
            .order_by_asc(hyperlink_artifact::Column::Id)
            .all(connection)
            .await
            .map_err(|err| format!("failed to load hyperlink artifacts: {err}"))?;

        let hyperlink_rows = hyperlinks
            .into_iter()
            .map(|model| HyperlinkBackupRow {
                id: model.id,
                title: model.title,
                url: model.url,
                raw_url: model.raw_url,
                og_title: model.og_title,
                og_description: model.og_description,
                og_type: model.og_type,
                og_url: model.og_url,
                og_image_url: model.og_image_url,
                og_site_name: model.og_site_name,
                discovery_depth: model.discovery_depth,
                clicks_count: model.clicks_count,
                last_clicked_at: model.last_clicked_at.as_ref().map(format_datetime),
                created_at: format_datetime(&model.created_at),
                updated_at: format_datetime(&model.updated_at),
            })
            .collect::<Vec<_>>();

        let relation_rows = relations
            .into_iter()
            .map(|model| HyperlinkRelationBackupRow {
                id: model.id,
                parent_hyperlink_id: model.parent_hyperlink_id,
                child_hyperlink_id: model.child_hyperlink_id,
                created_at: format_datetime(&model.created_at),
            })
            .collect::<Vec<_>>();

        let manifest = BackupManifest {
            version: BACKUP_VERSION,
            exported_at: format_datetime(&now_utc()),
            hyperlinks: hyperlink_rows.len(),
            relations: relation_rows.len(),
            artifacts: artifacts.len(),
        };

        let output_file = std::fs::File::create(output_path)
            .map_err(|err| format!("failed to create backup output file: {err}"))?;
        let mut writer = ZipWriter::new(output_file);
        write_zip_json_file(&mut writer, BACKUP_MANIFEST_PATH, &manifest)?;
        write_zip_json_file(&mut writer, BACKUP_HYPERLINKS_PATH, &hyperlink_rows)?;
        write_zip_json_file(&mut writer, BACKUP_RELATIONS_PATH, &relation_rows)?;

        let artifacts_total = artifacts.len();
        report_progress(BackupProgress {
            stage: BackupProgressStage::PackingArtifacts,
            artifacts_done: 0,
            artifacts_total,
        });

        let mut artifact_rows = Vec::with_capacity(artifacts_total);
        let mut payload_tasks = tokio::task::JoinSet::new();
        let mut next_submit_index = 0usize;
        let mut next_write_index = 0usize;
        let mut ready_payloads = HashMap::<usize, (hyperlink_artifact::Model, Vec<u8>)>::new();

        while next_submit_index < artifacts_total
            && payload_tasks.len() < BACKUP_ARTIFACT_READ_CONCURRENCY
        {
            let task_index = next_submit_index;
            let artifact = artifacts[task_index].clone();
            payload_tasks.spawn(async move {
                let artifact_id = artifact.id;
                let payload = hyperlink_artifact_model::load_payload(&artifact)
                    .await
                    .map_err(|err| {
                        format!("failed to load payload for artifact {artifact_id}: {err}")
                    })?;
                Ok::<_, String>((task_index, artifact, payload))
            });
            next_submit_index += 1;
        }

        while let Some(joined) = payload_tasks.join_next().await {
            let (task_index, artifact, payload) =
                joined.map_err(|err| format!("artifact payload task failed: {err}"))??;
            ready_payloads.insert(task_index, (artifact, payload));

            while let Some((artifact, payload)) = ready_payloads.remove(&next_write_index) {
                let payload_path = format!("{BACKUP_ARTIFACTS_DIR}/{}.bin", artifact.id);
                write_zip_binary_file_with_compression(
                    &mut writer,
                    &payload_path,
                    &payload,
                    CompressionMethod::Deflated,
                )?;

                artifact_rows.push(HyperlinkArtifactBackupRow {
                    id: artifact.id,
                    hyperlink_id: artifact.hyperlink_id,
                    kind: artifact.kind,
                    content_type: artifact.content_type,
                    size_bytes: artifact.size_bytes,
                    created_at: format_datetime(&artifact.created_at),
                    job_id: artifact.job_id,
                    checksum_sha256: artifact.checksum_sha256,
                    payload_path,
                });

                next_write_index += 1;
                report_progress(BackupProgress {
                    stage: BackupProgressStage::PackingArtifacts,
                    artifacts_done: artifact_rows.len(),
                    artifacts_total,
                });
            }

            while next_submit_index < artifacts_total
                && payload_tasks.len() < BACKUP_ARTIFACT_READ_CONCURRENCY
            {
                let task_index = next_submit_index;
                let artifact = artifacts[task_index].clone();
                payload_tasks.spawn(async move {
                    let artifact_id = artifact.id;
                    let payload = hyperlink_artifact_model::load_payload(&artifact)
                        .await
                        .map_err(|err| {
                            format!("failed to load payload for artifact {artifact_id}: {err}")
                        })?;
                    Ok::<_, String>((task_index, artifact, payload))
                });
                next_submit_index += 1;
            }
        }
        write_zip_json_file(&mut writer, BACKUP_ARTIFACTS_PATH, &artifact_rows)?;

        report_progress(BackupProgress {
            stage: BackupProgressStage::Finalizing,
            artifacts_done: artifact_rows.len(),
            artifacts_total,
        });

        writer
            .finish()
            .map_err(|err| format!("failed to finalize zip archive: {err}"))?;

        Ok(BuiltBackupZip {
            summary: BackupCompletionSummary {
                hyperlinks: hyperlink_rows.len(),
                relations: relation_rows.len(),
                artifacts: artifact_rows.len(),
            },
        })
    }
    .await;

    if build_result.is_err() {
        if let Err(error) = tokio::fs::remove_file(output_path).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %output_path.display(),
                error = %error,
                "failed to clean up incomplete backup archive"
            );
        }
    }

    build_result
}

fn write_zip_json_file<T: Serialize, W: Write + Seek>(
    writer: &mut ZipWriter<W>,
    path: &str,
    value: &T,
) -> Result<(), String> {
    let payload = serde_json::to_vec_pretty(value)
        .map_err(|err| format!("failed to encode {path}: {err}"))?;
    write_zip_binary_file_with_compression(writer, path, &payload, CompressionMethod::Deflated)
}

fn write_zip_binary_file_with_compression<W: Write + Seek>(
    writer: &mut ZipWriter<W>,
    path: &str,
    payload: &[u8],
    compression: CompressionMethod,
) -> Result<(), String> {
    let mut file_options = FileOptions::default().compression_method(compression);
    if compression == CompressionMethod::Deflated {
        file_options = file_options.compression_level(Some(BACKUP_DEFLATE_LEVEL_BEST));
    }
    writer
        .start_file(path, file_options)
        .map_err(|err| format!("failed to create {path} in zip archive: {err}"))?;
    writer
        .write_all(payload)
        .map_err(|err| format!("failed to write {path} in zip archive: {err}"))?;
    Ok(())
}

async fn read_uploaded_backup_archive(multipart: &mut Multipart) -> Result<PathBuf, String> {
    let mut seen_field_names: Vec<String> = Vec::new();

    while let Some(mut field) = multipart.next_field().await.map_err(|err| {
        tracing::warn!(error = %err, "failed to read multipart form for admin import");
        format!("failed to read multipart form: {err}")
    })? {
        let field_name = field.name().map(ToString::to_string);
        let file_name = field.file_name().map(ToString::to_string);
        let field_content_type = field.content_type().map(ToString::to_string);
        let field_name_label = field_name.as_deref().unwrap_or("<none>");

        seen_field_names.push(field_name_label.to_string());
        tracing::info!(
            field_name = %field_name_label,
            file_name = %file_name.as_deref().unwrap_or(""),
            field_content_type = %field_content_type.as_deref().unwrap_or(""),
            "received multipart field for admin import"
        );

        if field_name.as_deref() != Some("archive") {
            tracing::info!(
                field_name = %field_name_label,
                "ignoring multipart field for admin import because it is not `archive`"
            );
            continue;
        }

        let upload_path = next_import_upload_path();
        let mut file = tokio::fs::File::create(&upload_path)
            .await
            .map_err(|err| format!("failed to open temporary upload file: {err}"))?;

        let mut bytes_written: u64 = 0;
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|err| format!("failed to read uploaded zip file chunk: {err}"))?
        {
            file.write_all(&chunk)
                .await
                .map_err(|err| format!("failed to write uploaded zip file chunk: {err}"))?;
            bytes_written = bytes_written.saturating_add(chunk.len() as u64);
        }

        if let Err(err) = file.flush().await {
            remove_uploaded_backup_file(&upload_path).await;
            return Err(format!("failed to finalize uploaded zip file: {err}"));
        }

        if bytes_written == 0 {
            tracing::warn!(
                field_name = %field_name_label,
                file_name = %file_name.as_deref().unwrap_or(""),
                "uploaded admin import archive file is empty"
            );
            remove_uploaded_backup_file(&upload_path).await;
            return Err("uploaded backup ZIP file is empty".to_string());
        }

        tracing::info!(
            field_name = %field_name_label,
            file_name = %file_name.as_deref().unwrap_or(""),
            bytes_written,
            "accepted backup archive upload for admin import"
        );
        return Ok(upload_path);
    }

    tracing::warn!(
        seen_field_names = %seen_field_names.join(","),
        hint = "multipart body did not include an `archive` file part; ensure the upload input is enabled when submitting",
        "admin import upload did not include required multipart field `archive`"
    );
    Err("no backup ZIP file uploaded (expected form field `archive`)".to_string())
}

async fn remove_uploaded_backup_file(path: &Path) {
    if let Err(error) = tokio::fs::remove_file(path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(
            path = %path.display(),
            error = %error,
            "failed to remove uploaded backup file"
        );
    }
}

async fn import_backup_zip<F>(
    connection: &DatabaseConnection,
    archive_path: &Path,
    mut report_progress: F,
) -> Result<AdminImportSummary, String>
where
    F: FnMut(ImportProgress),
{
    let file = std::fs::File::open(archive_path)
        .map_err(|err| format!("failed to open uploaded zip archive: {err}"))?;
    let mut zip_archive =
        ZipArchive::new(file).map_err(|err| format!("invalid zip archive: {err}"))?;

    report_progress(ImportProgress {
        stage: ImportProgressStage::Validating,
        hyperlinks_done: 0,
        hyperlinks_total: 0,
        relations_done: 0,
        relations_total: 0,
        artifacts_done: 0,
        artifacts_total: 0,
    });

    let manifest: BackupManifest = read_zip_json_file(&mut zip_archive, BACKUP_MANIFEST_PATH)?;
    if manifest.version != BACKUP_VERSION {
        return Err(format!(
            "unsupported backup version {}; expected {}",
            manifest.version, BACKUP_VERSION
        ));
    }

    let hyperlinks: Vec<HyperlinkBackupRow> =
        read_zip_json_file(&mut zip_archive, BACKUP_HYPERLINKS_PATH)?;
    let relations: Vec<HyperlinkRelationBackupRow> =
        read_zip_json_file(&mut zip_archive, BACKUP_RELATIONS_PATH)?;
    let artifacts: Vec<HyperlinkArtifactBackupRow> =
        read_zip_json_file(&mut zip_archive, BACKUP_ARTIFACTS_PATH)?;

    if hyperlinks.len() != manifest.hyperlinks {
        return Err(format!(
            "hyperlinks count mismatch: manifest says {}, file contains {}",
            manifest.hyperlinks,
            hyperlinks.len()
        ));
    }
    if relations.len() != manifest.relations {
        return Err(format!(
            "relations count mismatch: manifest says {}, file contains {}",
            manifest.relations,
            relations.len()
        ));
    }
    if artifacts.len() != manifest.artifacts {
        return Err(format!(
            "artifacts count mismatch: manifest says {}, file contains {}",
            manifest.artifacts,
            artifacts.len()
        ));
    }

    let mut seen_artifact_ids = HashSet::with_capacity(artifacts.len());
    for artifact in &artifacts {
        if !seen_artifact_ids.insert(artifact.id) {
            return Err(format!(
                "duplicate artifact id {} in artifacts.json",
                artifact.id
            ));
        }
        validate_archive_entry_path(&artifact.payload_path)?;
        let _entry = zip_archive
            .by_name(&artifact.payload_path)
            .map_err(|err| format!("missing {} in backup zip: {err}", artifact.payload_path))?;
    }

    let parsed_archive = AdminBackupArchive {
        hyperlinks,
        relations,
        artifacts,
    };

    restore_backup_archive(
        connection,
        &mut zip_archive,
        parsed_archive,
        report_progress,
    )
    .await
}

fn read_zip_json_file<T: DeserializeOwned, R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    path: &str,
) -> Result<T, String> {
    let mut entry = archive
        .by_name(path)
        .map_err(|err| format!("missing {path} in backup zip: {err}"))?;
    let mut content = String::new();
    entry
        .read_to_string(&mut content)
        .map_err(|err| format!("failed to read {path} from backup zip: {err}"))?;
    serde_json::from_str(&content)
        .map_err(|err| format!("failed to parse {path} from backup zip: {err}"))
}

fn read_zip_binary_file<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    path: &str,
) -> Result<Vec<u8>, String> {
    let mut entry = archive
        .by_name(path)
        .map_err(|err| format!("missing {path} in backup zip: {err}"))?;
    let mut payload = Vec::new();
    entry
        .read_to_end(&mut payload)
        .map_err(|err| format!("failed to read {path} from backup zip: {err}"))?;
    Ok(payload)
}

async fn restore_backup_archive<F, R>(
    connection: &DatabaseConnection,
    zip_archive: &mut ZipArchive<R>,
    archive: AdminBackupArchive,
    mut report_progress: F,
) -> Result<AdminImportSummary, String>
where
    F: FnMut(ImportProgress),
    R: Read + Seek,
{
    let hyperlink_total = archive.hyperlinks.len();
    let relation_total = archive.relations.len();
    let artifact_total = archive.artifacts.len();
    let mut summary = AdminImportSummary::default();

    let mut hyperlinks = archive.hyperlinks;
    hyperlinks.sort_by_key(|row| row.id);
    report_progress(ImportProgress {
        stage: ImportProgressStage::RestoringHyperlinks,
        hyperlinks_done: 0,
        hyperlinks_total: hyperlink_total,
        relations_done: 0,
        relations_total: relation_total,
        artifacts_done: 0,
        artifacts_total: artifact_total,
    });
    for (index, row) in hyperlinks.iter().enumerate() {
        restore_hyperlink_row(connection, row).await?;
        summary.hyperlinks += 1;
        report_progress(ImportProgress {
            stage: ImportProgressStage::RestoringHyperlinks,
            hyperlinks_done: index + 1,
            hyperlinks_total: hyperlink_total,
            relations_done: 0,
            relations_total: relation_total,
            artifacts_done: 0,
            artifacts_total: artifact_total,
        });
    }

    let mut relations = archive.relations;
    relations.sort_by_key(|row| row.id);
    report_progress(ImportProgress {
        stage: ImportProgressStage::RestoringRelations,
        hyperlinks_done: summary.hyperlinks,
        hyperlinks_total: hyperlink_total,
        relations_done: 0,
        relations_total: relation_total,
        artifacts_done: 0,
        artifacts_total: artifact_total,
    });
    for (index, row) in relations.iter().enumerate() {
        restore_relation_row(connection, row).await?;
        summary.relations += 1;
        report_progress(ImportProgress {
            stage: ImportProgressStage::RestoringRelations,
            hyperlinks_done: summary.hyperlinks,
            hyperlinks_total: hyperlink_total,
            relations_done: index + 1,
            relations_total: relation_total,
            artifacts_done: 0,
            artifacts_total: artifact_total,
        });
    }

    let mut artifacts = archive.artifacts;
    artifacts.sort_by_key(|row| row.id);
    report_progress(ImportProgress {
        stage: ImportProgressStage::RestoringArtifacts,
        hyperlinks_done: summary.hyperlinks,
        hyperlinks_total: hyperlink_total,
        relations_done: summary.relations,
        relations_total: relation_total,
        artifacts_done: 0,
        artifacts_total: artifact_total,
    });
    for (index, row) in artifacts.iter().enumerate() {
        let payload = read_zip_binary_file(zip_archive, &row.payload_path)?;
        restore_artifact_row(connection, row, &payload).await?;
        summary.artifacts += 1;
        report_progress(ImportProgress {
            stage: ImportProgressStage::RestoringArtifacts,
            hyperlinks_done: summary.hyperlinks,
            hyperlinks_total: hyperlink_total,
            relations_done: summary.relations,
            relations_total: relation_total,
            artifacts_done: index + 1,
            artifacts_total: artifact_total,
        });
    }

    report_progress(ImportProgress {
        stage: ImportProgressStage::Finalizing,
        hyperlinks_done: summary.hyperlinks,
        hyperlinks_total: hyperlink_total,
        relations_done: summary.relations,
        relations_total: relation_total,
        artifacts_done: summary.artifacts,
        artifacts_total: artifact_total,
    });

    Ok(summary)
}

async fn restore_hyperlink_row(
    connection: &DatabaseConnection,
    row: &HyperlinkBackupRow,
) -> Result<(), String> {
    let created_at = parse_datetime(&row.created_at)
        .map_err(|err| format!("invalid created_at for hyperlink {}: {err}", row.id))?;
    let updated_at = parse_datetime(&row.updated_at)
        .map_err(|err| format!("invalid updated_at for hyperlink {}: {err}", row.id))?;
    let last_clicked_at = parse_optional_datetime(row.last_clicked_at.as_deref())
        .map_err(|err| format!("invalid last_clicked_at for hyperlink {}: {err}", row.id))?;

    if let Some(existing) = hyperlink::Entity::find_by_id(row.id)
        .one(connection)
        .await
        .map_err(|err| format!("failed to load hyperlink {}: {err}", row.id))?
    {
        let mut active: hyperlink::ActiveModel = existing.into();
        active.title = Set(row.title.clone());
        active.url = Set(row.url.clone());
        active.raw_url = Set(row.raw_url.clone());
        active.og_title = Set(row.og_title.clone());
        active.og_description = Set(row.og_description.clone());
        active.og_type = Set(row.og_type.clone());
        active.og_url = Set(row.og_url.clone());
        active.og_image_url = Set(row.og_image_url.clone());
        active.og_site_name = Set(row.og_site_name.clone());
        active.discovery_depth = Set(row.discovery_depth);
        active.clicks_count = Set(row.clicks_count);
        active.last_clicked_at = Set(last_clicked_at);
        active.created_at = Set(created_at);
        active.updated_at = Set(updated_at);
        active
            .update(connection)
            .await
            .map_err(|err| format!("failed to update hyperlink {}: {err}", row.id))?;
    } else {
        hyperlink::ActiveModel {
            id: Set(row.id),
            title: Set(row.title.clone()),
            url: Set(row.url.clone()),
            raw_url: Set(row.raw_url.clone()),
            og_title: Set(row.og_title.clone()),
            og_description: Set(row.og_description.clone()),
            og_type: Set(row.og_type.clone()),
            og_url: Set(row.og_url.clone()),
            og_image_url: Set(row.og_image_url.clone()),
            og_site_name: Set(row.og_site_name.clone()),
            discovery_depth: Set(row.discovery_depth),
            clicks_count: Set(row.clicks_count),
            last_clicked_at: Set(last_clicked_at),
            created_at: Set(created_at),
            updated_at: Set(updated_at),
            ..Default::default()
        }
        .insert(connection)
        .await
        .map_err(|err| format!("failed to insert hyperlink {}: {err}", row.id))?;
    }

    Ok(())
}

async fn restore_relation_row(
    connection: &DatabaseConnection,
    row: &HyperlinkRelationBackupRow,
) -> Result<(), String> {
    let created_at = parse_datetime(&row.created_at)
        .map_err(|err| format!("invalid created_at for relation {}: {err}", row.id))?;

    if let Some(existing) = hyperlink_relation::Entity::find_by_id(row.id)
        .one(connection)
        .await
        .map_err(|err| format!("failed to load relation {}: {err}", row.id))?
    {
        let mut active: hyperlink_relation::ActiveModel = existing.into();
        active.parent_hyperlink_id = Set(row.parent_hyperlink_id);
        active.child_hyperlink_id = Set(row.child_hyperlink_id);
        active.created_at = Set(created_at);
        active
            .update(connection)
            .await
            .map_err(|err| format!("failed to update relation {}: {err}", row.id))?;
        return Ok(());
    }

    let insert_result = hyperlink_relation::ActiveModel {
        id: Set(row.id),
        parent_hyperlink_id: Set(row.parent_hyperlink_id),
        child_hyperlink_id: Set(row.child_hyperlink_id),
        created_at: Set(created_at),
        ..Default::default()
    }
    .insert(connection)
    .await;

    if let Err(err) = insert_result {
        if let Some(existing) = hyperlink_relation::Entity::find()
            .filter(hyperlink_relation::Column::ParentHyperlinkId.eq(row.parent_hyperlink_id))
            .filter(hyperlink_relation::Column::ChildHyperlinkId.eq(row.child_hyperlink_id))
            .one(connection)
            .await
            .map_err(|load_err| {
                format!(
                    "failed to load existing relation ({}, {}): {load_err}",
                    row.parent_hyperlink_id, row.child_hyperlink_id
                )
            })?
        {
            let mut active: hyperlink_relation::ActiveModel = existing.into();
            active.created_at = Set(created_at);
            active.update(connection).await.map_err(|update_err| {
                format!(
                    "failed to update existing relation ({}, {}): {update_err}",
                    row.parent_hyperlink_id, row.child_hyperlink_id
                )
            })?;
            return Ok(());
        }
        return Err(format!("failed to insert relation {}: {err}", row.id));
    }

    Ok(())
}

async fn restore_artifact_row(
    connection: &DatabaseConnection,
    row: &HyperlinkArtifactBackupRow,
    payload: &[u8],
) -> Result<(), String> {
    let created_at = parse_datetime(&row.created_at)
        .map_err(|err| format!("invalid created_at for artifact {}: {err}", row.id))?;
    let size_bytes = i32::try_from(payload.len())
        .map_err(|_| format!("artifact {} payload exceeds i32::MAX bytes", row.id))?;
    let stored = artifact_storage::write_payload(row.hyperlink_id, &row.kind, created_at, payload)
        .await
        .map_err(|err| format!("failed to write payload for artifact {}: {err}", row.id))?;

    if let Some(existing) = hyperlink_artifact::Entity::find_by_id(row.id)
        .one(connection)
        .await
        .map_err(|err| format!("failed to load artifact {}: {err}", row.id))?
    {
        if let Some(old_path) = existing.storage_path.as_deref()
            && let Err(err) = artifact_storage::delete_if_exists(old_path).await
        {
            tracing::warn!(
                artifact_id = existing.id,
                storage_path = old_path,
                error = %err,
                "failed to delete existing artifact payload before restore"
            );
        }

        let mut active: hyperlink_artifact::ActiveModel = existing.into();
        active.hyperlink_id = Set(row.hyperlink_id);
        active.job_id = Set(None);
        active.kind = Set(row.kind.clone());
        active.payload = Set(Vec::new());
        active.storage_path = Set(Some(stored.storage_path));
        active.storage_backend = Set(Some(artifact_storage::DISK_STORAGE_BACKEND.to_string()));
        active.checksum_sha256 = Set(Some(stored.checksum_sha256));
        active.content_type = Set(row.content_type.clone());
        active.size_bytes = Set(size_bytes);
        active.created_at = Set(created_at);
        active
            .update(connection)
            .await
            .map_err(|err| format!("failed to update artifact {}: {err}", row.id))?;
    } else {
        hyperlink_artifact::ActiveModel {
            id: Set(row.id),
            hyperlink_id: Set(row.hyperlink_id),
            job_id: Set(None),
            kind: Set(row.kind.clone()),
            payload: Set(Vec::new()),
            storage_path: Set(Some(stored.storage_path)),
            storage_backend: Set(Some(artifact_storage::DISK_STORAGE_BACKEND.to_string())),
            checksum_sha256: Set(Some(stored.checksum_sha256)),
            content_type: Set(row.content_type.clone()),
            size_bytes: Set(size_bytes),
            created_at: Set(created_at),
        }
        .insert(connection)
        .await
        .map_err(|err| format!("failed to insert artifact {}: {err}", row.id))?;
    }

    Ok(())
}

fn validate_archive_entry_path(path: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("artifact payload path is empty".to_string());
    }

    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return Err(format!("artifact payload path must be relative: {path}"));
    }

    for component in candidate.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(format!("artifact payload path is unsafe: {path}"));
        }
    }

    Ok(())
}

fn now_utc() -> sea_orm::entity::prelude::DateTime {
    sea_orm::entity::prelude::DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}

fn format_datetime(value: &sea_orm::entity::prelude::DateTime) -> String {
    value.format("%Y-%m-%dT%H:%M:%S%.fZ").to_string()
}

fn parse_optional_datetime(
    value: Option<&str>,
) -> Result<Option<sea_orm::entity::prelude::DateTime>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    parse_datetime(value).map(Some)
}

fn parse_datetime(value: &str) -> Result<sea_orm::entity::prelude::DateTime, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("value is empty".to_string());
    }

    let value = value.strip_suffix('Z').unwrap_or(value);
    parse_naive_datetime(value).ok_or_else(|| format!("unsupported datetime format: {value}"))
}

fn parse_naive_datetime(value: &str) -> Option<sea_orm::entity::prelude::DateTime> {
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
    ] {
        if let Ok(parsed) = sea_orm::entity::prelude::DateTime::parse_from_str(value, format) {
            return Some(parsed);
        }
    }
    None
}

fn checkbox_checked(value: &Option<String>) -> bool {
    value.is_some()
}

fn build_artifact_settings_message(
    deleted_source: u64,
    deleted_screenshots: u64,
    deleted_og: u64,
    deleted_readability: u64,
    queued_snapshot: usize,
    queued_screenshots: usize,
    queued_og: usize,
    queued_readability: usize,
    queue_warning: Option<&str>,
) -> String {
    let mut parts = vec!["Updated artifact settings.".to_string()];

    if deleted_source > 0 || deleted_screenshots > 0 || deleted_og > 0 || deleted_readability > 0 {
        parts.push(format!(
            "Deleted source={deleted_source}, screenshots={deleted_screenshots}, og={deleted_og}, readability={deleted_readability}."
        ));
    }

    if queued_snapshot > 0 || queued_screenshots > 0 || queued_og > 0 || queued_readability > 0 {
        parts.push(format!(
            "Queued backfill snapshot={queued_snapshot}, screenshots={queued_screenshots}, og={queued_og}, readability={queued_readability}."
        ));
    }

    if let Some(warning) = queue_warning {
        parts.push(warning.to_string());
    }

    parts.join(" ")
}

async fn enqueue_hyperlink_jobs(
    connection: &DatabaseConnection,
    queue: Option<&ProcessingQueueSender>,
    kind: HyperlinkProcessingJobKind,
    hyperlink_ids: &[i32],
) -> Result<usize, DbErr> {
    let artifact_settings = settings::load(connection).await?;
    let mut queued = 0usize;
    for hyperlink_id in hyperlink_ids {
        if matches!(
            kind,
            HyperlinkProcessingJobKind::Snapshot
                | HyperlinkProcessingJobKind::Og
                | HyperlinkProcessingJobKind::Readability
        ) {
            let result = artifact_job::resolve_and_enqueue_for_job_kind_with_settings(
                connection,
                *hyperlink_id,
                kind.clone(),
                ArtifactFetchMode::EnsurePresent,
                artifact_settings,
                queue,
            )
            .await?;
            if result.was_enqueued() {
                queued += 1;
            }
            continue;
        }

        hyperlink_processing_job_model::enqueue_for_hyperlink_kind(
            connection,
            *hyperlink_id,
            kind.clone(),
            queue,
        )
        .await?;
        queued += 1;
    }
    Ok(queued)
}

async fn delete_source_artifacts(connection: &DatabaseConnection) -> Result<u64, DbErr> {
    delete_artifacts_for_kinds(
        connection,
        &[
            HyperlinkArtifactKind::SnapshotWarc,
            HyperlinkArtifactKind::PdfSource,
            HyperlinkArtifactKind::SnapshotError,
        ],
    )
    .await
}

async fn delete_screenshot_artifacts(connection: &DatabaseConnection) -> Result<u64, DbErr> {
    delete_artifacts_for_kinds(
        connection,
        &[
            HyperlinkArtifactKind::ScreenshotWebp,
            HyperlinkArtifactKind::ScreenshotThumbWebp,
            HyperlinkArtifactKind::ScreenshotDarkWebp,
            HyperlinkArtifactKind::ScreenshotThumbDarkWebp,
            HyperlinkArtifactKind::ScreenshotError,
        ],
    )
    .await
}

async fn delete_og_artifacts_and_clear_fields(
    connection: &DatabaseConnection,
) -> Result<u64, DbErr> {
    let deleted = delete_artifacts_for_kinds(
        connection,
        &[
            HyperlinkArtifactKind::OgMeta,
            HyperlinkArtifactKind::OgImage,
            HyperlinkArtifactKind::OgError,
        ],
    )
    .await?;
    clear_hyperlink_og_fields(connection).await?;
    Ok(deleted)
}

async fn delete_readability_artifacts_and_clear_search(
    connection: &DatabaseConnection,
) -> Result<u64, DbErr> {
    let deleted = delete_artifacts_for_kinds(
        connection,
        &[
            HyperlinkArtifactKind::ReadableText,
            HyperlinkArtifactKind::ReadableMeta,
            HyperlinkArtifactKind::ReadableError,
        ],
    )
    .await?;

    match hyperlink_search_doc::clear_all_readable_text(connection).await {
        Ok(_) => {}
        Err(error) if hyperlink_search_doc::is_search_doc_missing_error(&error) => {}
        Err(error) => return Err(error),
    }

    Ok(deleted)
}

async fn delete_artifacts_for_kinds(
    connection: &DatabaseConnection,
    kinds: &[HyperlinkArtifactKind],
) -> Result<u64, DbErr> {
    if kinds.is_empty() {
        return Ok(0);
    }

    let result = hyperlink_artifact::Entity::delete_many()
        .filter(hyperlink_artifact::Column::Kind.is_in(kinds.to_vec()))
        .exec(connection)
        .await?;
    Ok(result.rows_affected)
}

async fn clear_hyperlink_og_fields(connection: &DatabaseConnection) -> Result<u64, DbErr> {
    let backend = connection.get_database_backend();
    let result = connection
        .execute_raw(Statement::from_string(
            backend,
            r#"
                UPDATE hyperlink
                SET
                    og_title = NULL,
                    og_description = NULL,
                    og_type = NULL,
                    og_url = NULL,
                    og_image_url = NULL,
                    og_site_name = NULL,
                    updated_at = CURRENT_TIMESTAMP
                WHERE
                    og_title IS NOT NULL
                    OR og_description IS NOT NULL
                    OR og_type IS NOT NULL
                    OR og_url IS NOT NULL
                    OR og_image_url IS NOT NULL
                    OR og_site_name IS NOT NULL
            "#
            .to_string(),
        ))
        .await?;
    Ok(result.rows_affected())
}

async fn build_missing_artifacts_plan(
    connection: &DatabaseConnection,
    artifact_settings: &ArtifactCollectionSettings,
) -> Result<MissingArtifactsPlan, sea_orm::DbErr> {
    let hyperlink_ids = hyperlink::Entity::find()
        .select_only()
        .column(hyperlink::Column::Id)
        .into_tuple::<i32>()
        .all(connection)
        .await?;

    let mut summary = MissingArtifactsSummary {
        total_hyperlinks: hyperlink_ids.len(),
        ..Default::default()
    };

    if hyperlink_ids.is_empty() {
        return Ok(MissingArtifactsPlan {
            summary,
            snapshot_hyperlink_ids: Vec::new(),
            screenshot_hyperlink_ids: Vec::new(),
            og_hyperlink_ids: Vec::new(),
            readability_hyperlink_ids: Vec::new(),
        });
    }

    let artifact_rows = hyperlink_artifact::Entity::find()
        .select_only()
        .column(hyperlink_artifact::Column::HyperlinkId)
        .column(hyperlink_artifact::Column::Kind)
        .filter(hyperlink_artifact::Column::HyperlinkId.is_in(hyperlink_ids.clone()))
        .filter(hyperlink_artifact::Column::Kind.is_in([
            HyperlinkArtifactKind::SnapshotWarc,
            HyperlinkArtifactKind::PdfSource,
            HyperlinkArtifactKind::ScreenshotWebp,
            HyperlinkArtifactKind::ScreenshotThumbWebp,
            HyperlinkArtifactKind::ScreenshotDarkWebp,
            HyperlinkArtifactKind::ScreenshotThumbDarkWebp,
            HyperlinkArtifactKind::OgMeta,
            HyperlinkArtifactKind::ReadableText,
            HyperlinkArtifactKind::ReadableMeta,
        ]))
        .into_tuple::<(i32, HyperlinkArtifactKind)>()
        .all(connection)
        .await?;

    let mut artifact_presence_by_hyperlink = HashMap::<i32, ArtifactPresence>::new();
    for (hyperlink_id, kind) in artifact_rows {
        let presence = artifact_presence_by_hyperlink
            .entry(hyperlink_id)
            .or_default();
        match kind {
            HyperlinkArtifactKind::SnapshotWarc | HyperlinkArtifactKind::PdfSource => {
                presence.has_source = true;
            }
            HyperlinkArtifactKind::ScreenshotWebp
            | HyperlinkArtifactKind::ScreenshotThumbWebp
            | HyperlinkArtifactKind::ScreenshotDarkWebp
            | HyperlinkArtifactKind::ScreenshotThumbDarkWebp => presence.has_screenshot = true,
            HyperlinkArtifactKind::OgMeta => {
                presence.has_og_meta = true;
            }
            HyperlinkArtifactKind::ReadableText => {
                presence.has_readable_text = true;
            }
            HyperlinkArtifactKind::ReadableMeta => {
                presence.has_readable_meta = true;
            }
            _ => {}
        }
    }

    let active_rows = hyperlink_processing_job::Entity::find()
        .select_only()
        .column(hyperlink_processing_job::Column::HyperlinkId)
        .column(hyperlink_processing_job::Column::Kind)
        .filter(hyperlink_processing_job::Column::HyperlinkId.is_in(hyperlink_ids.clone()))
        .filter(hyperlink_processing_job::Column::State.is_in([
            HyperlinkProcessingJobState::Queued,
            HyperlinkProcessingJobState::Running,
        ]))
        .filter(hyperlink_processing_job::Column::Kind.is_in([
            HyperlinkProcessingJobKind::Snapshot,
            HyperlinkProcessingJobKind::Og,
            HyperlinkProcessingJobKind::Readability,
        ]))
        .into_tuple::<(i32, HyperlinkProcessingJobKind)>()
        .all(connection)
        .await?;

    let mut snapshot_active_hyperlinks = HashSet::<i32>::new();
    let mut og_active_hyperlinks = HashSet::<i32>::new();
    let mut readability_active_hyperlinks = HashSet::<i32>::new();
    for (hyperlink_id, kind) in active_rows {
        match kind {
            HyperlinkProcessingJobKind::Snapshot => {
                snapshot_active_hyperlinks.insert(hyperlink_id);
            }
            HyperlinkProcessingJobKind::Og => {
                og_active_hyperlinks.insert(hyperlink_id);
            }
            HyperlinkProcessingJobKind::Readability => {
                readability_active_hyperlinks.insert(hyperlink_id);
            }
            _ => {}
        }
    }

    let mut snapshot_hyperlink_ids = Vec::new();
    let mut screenshot_hyperlink_ids = Vec::new();
    let mut og_hyperlink_ids = Vec::new();
    let mut readability_hyperlink_ids = Vec::new();

    for hyperlink_id in hyperlink_ids {
        let presence = artifact_presence_by_hyperlink.get(&hyperlink_id);
        let has_source = presence.is_some_and(|presence| presence.has_source);
        if !has_source {
            summary.missing_source += 1;
            if artifact_settings.collect_source {
                if snapshot_active_hyperlinks.contains(&hyperlink_id) {
                    summary.snapshot_already_processing += 1;
                } else {
                    snapshot_hyperlink_ids.push(hyperlink_id);
                }
            }
            continue;
        }

        let has_screenshot = presence.is_some_and(|presence| presence.has_screenshot);
        if artifact_settings.collect_screenshots
            && !has_screenshot
            && !snapshot_active_hyperlinks.contains(&hyperlink_id)
        {
            screenshot_hyperlink_ids.push(hyperlink_id);
        }

        let has_og_meta = presence.is_some_and(|presence| presence.has_og_meta);
        if !has_og_meta {
            summary.missing_og += 1;
            if artifact_settings.collect_og {
                if og_active_hyperlinks.contains(&hyperlink_id) {
                    summary.og_already_processing += 1;
                } else {
                    og_hyperlink_ids.push(hyperlink_id);
                }
            }
        }

        let has_readable_artifacts = presence
            .is_some_and(|presence| presence.has_readable_text && presence.has_readable_meta);
        if !has_readable_artifacts {
            summary.missing_readability += 1;
            if artifact_settings.collect_readability {
                if readability_active_hyperlinks.contains(&hyperlink_id) {
                    summary.readability_already_processing += 1;
                } else {
                    readability_hyperlink_ids.push(hyperlink_id);
                }
            }
        }
    }

    summary.snapshot_will_queue = snapshot_hyperlink_ids.len();
    summary.og_will_queue = og_hyperlink_ids.len();
    summary.readability_will_queue = readability_hyperlink_ids.len();

    Ok(MissingArtifactsPlan {
        summary,
        snapshot_hyperlink_ids,
        screenshot_hyperlink_ids,
        og_hyperlink_ids,
        readability_hyperlink_ids,
    })
}

async fn execute_missing_artifacts_plan(
    connection: &DatabaseConnection,
    queue: Option<&ProcessingQueueSender>,
    artifact_settings: &ArtifactCollectionSettings,
    plan: MissingArtifactsPlan,
) -> Result<LastRunSummary, sea_orm::DbErr> {
    let snapshot_queued = if artifact_settings.collect_source {
        enqueue_hyperlink_jobs(
            connection,
            queue,
            HyperlinkProcessingJobKind::Snapshot,
            &plan.snapshot_hyperlink_ids,
        )
        .await?
    } else {
        0
    };
    let og_queued = if artifact_settings.collect_og {
        enqueue_hyperlink_jobs(
            connection,
            queue,
            HyperlinkProcessingJobKind::Og,
            &plan.og_hyperlink_ids,
        )
        .await?
    } else {
        0
    };
    let readability_queued = if artifact_settings.collect_readability {
        enqueue_hyperlink_jobs(
            connection,
            queue,
            HyperlinkProcessingJobKind::Readability,
            &plan.readability_hyperlink_ids,
        )
        .await?
    } else {
        0
    };

    Ok(LastRunSummary {
        snapshot_queued,
        og_queued,
        readability_queued,
    })
}

async fn build_missing_screenshot_hyperlink_ids(
    connection: &DatabaseConnection,
) -> Result<Vec<i32>, DbErr> {
    let plan = build_missing_artifacts_plan(
        connection,
        &ArtifactCollectionSettings {
            collect_source: true,
            collect_screenshots: true,
            collect_screenshot_dark: true,
            collect_og: false,
            collect_readability: false,
        },
    )
    .await?;
    Ok(plan.screenshot_hyperlink_ids)
}

async fn build_dataset_stats(
    connection: &DatabaseConnection,
) -> Result<AdminDatasetStats, sea_orm::DbErr> {
    let total_hyperlinks = hyperlink::Entity::find().count(connection).await? as usize;
    let root_hyperlinks = hyperlink::Entity::find()
        .filter(hyperlink::Column::DiscoveryDepth.eq(0))
        .count(connection)
        .await? as usize;
    let discovered_hyperlinks = total_hyperlinks.saturating_sub(root_hyperlinks);

    let total_artifacts = hyperlink_artifact::Entity::find().count(connection).await? as usize;
    let total_processing_jobs = hyperlink_processing_job::Entity::find()
        .count(connection)
        .await? as usize;
    let active_processing_jobs = hyperlink_processing_job::Entity::find()
        .filter(hyperlink_processing_job::Column::State.is_in([
            HyperlinkProcessingJobState::Queued,
            HyperlinkProcessingJobState::Running,
        ]))
        .count(connection)
        .await? as usize;
    let sqlite_disk_stats = load_sqlite_disk_stats(connection).await?;
    let artifact_storage_breakdown = load_artifact_storage_breakdown(connection).await?;

    Ok(AdminDatasetStats {
        total_hyperlinks,
        root_hyperlinks,
        discovered_hyperlinks,
        total_artifacts,
        total_processing_jobs,
        active_processing_jobs,
        db_size_total_bytes: sqlite_disk_stats.total_bytes(),
        db_size_main_bytes: sqlite_disk_stats.main_bytes,
        db_size_wal_bytes: sqlite_disk_stats.wal_bytes,
        db_size_shm_bytes: sqlite_disk_stats.shm_bytes,
        saved_artifacts_size_bytes: artifact_storage_breakdown.saved_total_bytes,
        saved_artifacts_count: artifact_storage_breakdown.saved_total_count,
        discovered_artifacts_size_bytes: artifact_storage_breakdown.discovered_total_bytes,
        discovered_artifacts_count: artifact_storage_breakdown.discovered_total_count,
        artifact_storage_by_kind: artifact_storage_breakdown.by_kind,
    })
}

async fn load_artifact_storage_breakdown(
    connection: &DatabaseConnection,
) -> Result<ArtifactStorageBreakdown, DbErr> {
    let backend = connection.get_database_backend();
    let rows = connection
        .query_all_raw(Statement::from_string(
            backend,
            r#"
                SELECT
                    a.kind AS kind,
                    COALESCE(SUM(CASE WHEN h.discovery_depth = 0 THEN a.size_bytes ELSE 0 END), 0) AS saved_size_bytes,
                    COALESCE(SUM(CASE WHEN h.discovery_depth = 0 THEN 1 ELSE 0 END), 0) AS saved_artifact_count,
                    COALESCE(SUM(CASE WHEN h.discovery_depth > 0 THEN a.size_bytes ELSE 0 END), 0) AS discovered_size_bytes,
                    COALESCE(SUM(CASE WHEN h.discovery_depth > 0 THEN 1 ELSE 0 END), 0) AS discovered_artifact_count
                FROM hyperlink_artifact a
                INNER JOIN hyperlink h
                    ON h.id = a.hyperlink_id
                GROUP BY a.kind
                ORDER BY (saved_size_bytes + discovered_size_bytes) DESC, a.kind ASC
            "#
            .to_string(),
        ))
        .await?;

    let mut breakdown = ArtifactStorageBreakdown::default();
    for row in rows {
        let kind: String = row.try_get("", "kind")?;
        let saved_size_bytes: i64 = row.try_get("", "saved_size_bytes")?;
        let saved_artifact_count: i64 = row.try_get("", "saved_artifact_count")?;
        let discovered_size_bytes: i64 = row.try_get("", "discovered_size_bytes")?;
        let discovered_artifact_count: i64 = row.try_get("", "discovered_artifact_count")?;

        let saved_size_bytes = saved_size_bytes.max(0);
        let saved_artifact_count = saved_artifact_count.max(0);
        let discovered_size_bytes = discovered_size_bytes.max(0);
        let discovered_artifact_count = discovered_artifact_count.max(0);

        breakdown.saved_total_bytes = breakdown.saved_total_bytes.saturating_add(saved_size_bytes);
        breakdown.saved_total_count = breakdown
            .saved_total_count
            .saturating_add(saved_artifact_count);
        breakdown.discovered_total_bytes = breakdown
            .discovered_total_bytes
            .saturating_add(discovered_size_bytes);
        breakdown.discovered_total_count = breakdown
            .discovered_total_count
            .saturating_add(discovered_artifact_count);
        breakdown.by_kind.push(ArtifactStorageByKind {
            kind,
            saved_size_bytes,
            saved_artifact_count,
            discovered_size_bytes,
            discovered_artifact_count,
        });
    }

    Ok(breakdown)
}

async fn load_sqlite_disk_stats(connection: &DatabaseConnection) -> Result<SqliteDiskStats, DbErr> {
    let backend = connection.get_database_backend();
    let rows = connection
        .query_all_raw(Statement::from_string(
            backend,
            "PRAGMA database_list".to_string(),
        ))
        .await?;
    let main_file_path = rows.into_iter().find_map(|row| {
        let name: String = row.try_get("", "name").ok()?;
        if name != "main" {
            return None;
        }
        row.try_get::<String>("", "file").ok()
    });
    let Some(main_file_path) = main_file_path else {
        return Ok(SqliteDiskStats::default());
    };
    if main_file_path.trim().is_empty() {
        return Ok(SqliteDiskStats::default());
    }

    sqlite_disk_stats_from_main_path(Path::new(&main_file_path))
}

fn sqlite_disk_stats_from_main_path(main_path: &Path) -> Result<SqliteDiskStats, DbErr> {
    let wal_path = append_path_suffix(main_path, "-wal");
    let shm_path = append_path_suffix(main_path, "-shm");

    Ok(SqliteDiskStats {
        main_bytes: file_size_bytes(main_path)?,
        wal_bytes: file_size_bytes(&wal_path)?,
        shm_bytes: file_size_bytes(&shm_path)?,
    })
}

fn append_path_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut path_text = path.as_os_str().to_string_lossy().to_string();
    path_text.push_str(suffix);
    PathBuf::from(path_text)
}

fn file_size_bytes(path: &Path) -> Result<u64, DbErr> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(0),
        Err(err) => Err(DbErr::Custom(format!(
            "failed to read file metadata for {}: {err}",
            path.display()
        ))),
    }
}

#[derive(Template)]
#[template(path = "admin/index.stpl")]
struct AdminIndexTemplate<'a> {
    active_tab: AdminTab,
    app_version: &'a str,
    summary: &'a MissingArtifactsSummary,
    stats: &'a AdminDatasetStats,
    artifact_settings: &'a ArtifactCollectionSettings,
    llm_settings: &'a LlmSettings,
    llm_interactions: AdminLlmInteractionsPage,
    queue_pending: &'a crate::server::admin_jobs::QueuePendingCounts,
    has_missing_artifacts_to_process: bool,
    chromium: &'a ChromiumDiagnostics,
    fonts: &'a FontDiagnostics,
    mathpix: &'a MathpixStatus,
    mathpix_usage: &'a MathpixUsageSummary,
    rendered_at: chrono::DateTime<Utc>,
}

impl AdminIndexTemplate<'_> {
    fn active_tab_title(&self) -> &'static str {
        self.active_tab.title()
    }

    fn active_tab_summary(&self) -> &'static str {
        self.active_tab.summary()
    }

    fn is_overview_tab(&self) -> bool {
        self.active_tab == AdminTab::Overview
    }

    fn is_artifacts_tab(&self) -> bool {
        self.active_tab == AdminTab::Artifacts
    }

    fn is_llm_interactions_tab(&self) -> bool {
        self.active_tab == AdminTab::LlmInteractions
    }

    fn is_queue_tab(&self) -> bool {
        self.active_tab == AdminTab::Queue
    }

    fn is_import_export_tab(&self) -> bool {
        self.active_tab == AdminTab::ImportExport
    }

    fn is_storage_tab(&self) -> bool {
        self.active_tab == AdminTab::Storage
    }

    fn format_u64_bytes(&self, bytes: u64) -> String {
        format_bytes(bytes)
    }

    fn format_i64_bytes(&self, bytes: i64) -> String {
        format_bytes(bytes.max(0) as u64)
    }

    fn format_average_i64_bytes(&self, total_bytes: i64, count: i64) -> String {
        if count <= 0 {
            return "-".to_string();
        }
        let average_bytes = total_bytes.max(0) as f64 / count as f64;
        format_bytes_f64(average_bytes)
    }

    fn format_usd_estimate(&self, value: f64) -> String {
        format!("${value:.2}")
    }

    fn llm_interaction_context(&self, interaction: &llm_interaction::Model) -> String {
        let mut parts = Vec::new();

        if let Some(hyperlink_id) = interaction.hyperlink_id {
            parts.push(format!("hyperlink #{hyperlink_id}"));
        }
        if let Some(processing_job_id) = interaction.processing_job_id {
            parts.push(format!("processing job #{processing_job_id}"));
        }
        if let Some(admin_job_kind) = interaction.admin_job_kind.as_deref() {
            match interaction.admin_job_id {
                Some(admin_job_id) => parts.push(format!("{admin_job_kind} #{admin_job_id}")),
                None => parts.push(admin_job_kind.to_string()),
            }
        }

        if parts.is_empty() {
            "-".to_string()
        } else {
            parts.join(" | ")
        }
    }

    fn llm_interaction_status(&self, interaction: &llm_interaction::Model) -> String {
        match (
            interaction.response_status,
            interaction.error_message.as_deref(),
        ) {
            (Some(status), Some(error)) => format!("{status} | {error}"),
            (Some(status), None) => status.to_string(),
            (None, Some(error)) => error.to_string(),
            (None, None) => "-".to_string(),
        }
    }

    fn llm_interaction_duration(&self, interaction: &llm_interaction::Model) -> String {
        interaction
            .duration_ms
            .map(|duration_ms| format!("{duration_ms} ms"))
            .unwrap_or_else(|| "-".to_string())
    }

    fn llm_interaction_created_at(&self, interaction: &llm_interaction::Model) -> String {
        let created_at =
            chrono::DateTime::<Utc>::from_naive_utc_and_offset(interaction.created_at, Utc);
        let delta = self.rendered_at.signed_duration_since(created_at);

        if delta >= chrono::Duration::zero() && delta < chrono::Duration::hours(24) {
            return format_relative_duration(delta);
        }
        if delta < chrono::Duration::zero() && delta > -chrono::Duration::hours(24) {
            return format_relative_duration(delta);
        }

        interaction
            .created_at
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()
    }

    fn llm_interaction_created_at_title(&self, interaction: &llm_interaction::Model) -> String {
        interaction
            .created_at
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()
    }
}

async fn build_llm_interactions_page(
    connection: &DatabaseConnection,
    requested_page: u64,
) -> Result<AdminLlmInteractionsPage, DbErr> {
    let page =
        llm_interaction_model::list_page(connection, requested_page, LLM_INTERACTIONS_PER_PAGE)
            .await?;
    let prev_page_href = if page.page > 1 {
        Some(llm_interactions_href(page.page - 1))
    } else {
        None
    };
    let next_page_href = if page.page < page.total_pages {
        Some(llm_interactions_href(page.page + 1))
    } else {
        None
    };

    Ok(AdminLlmInteractionsPage {
        items: page.items,
        page: page.page,
        total_pages: page.total_pages,
        total_items: page.total_items,
        prev_page_href,
        next_page_href,
    })
}

fn render_index(
    active_tab: AdminTab,
    summary: &MissingArtifactsSummary,
    stats: &AdminDatasetStats,
    artifact_settings: &ArtifactCollectionSettings,
    llm_settings: &LlmSettings,
    llm_interactions: AdminLlmInteractionsPage,
    queue_pending: &crate::server::admin_jobs::QueuePendingCounts,
    chromium: &ChromiumDiagnostics,
    fonts: &FontDiagnostics,
    mathpix: &MathpixStatus,
    mathpix_usage: &MathpixUsageSummary,
) -> Result<String, sailfish::RenderError> {
    AdminIndexTemplate {
        active_tab,
        app_version: APP_VERSION,
        summary,
        stats,
        artifact_settings,
        llm_settings,
        llm_interactions,
        queue_pending,
        has_missing_artifacts_to_process: summary.snapshot_will_queue > 0
            || summary.og_will_queue > 0
            || summary.readability_will_queue > 0,
        chromium,
        fonts,
        mathpix,
        mathpix_usage,
        rendered_at: Utc::now(),
    }
    .render()
}

fn response_error(status: StatusCode, message: impl Into<String>) -> Response {
    views::render_error_page(status, message, ADMIN_TAB_ARTIFACTS_PATH, "Back to admin")
}

fn trimmed_non_empty(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
#[path = "../../../../tests/unit/app_controllers_admin_dashboard_controller.rs"]
mod tests;
