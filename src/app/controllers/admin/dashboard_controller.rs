use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
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
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, PaginatorTrait,
    QueryFilter, Statement,
};
use serde::{Deserialize, Serialize};
use tokio_util::io::ReaderStream;
#[cfg(test)]
use zip::{CompressionMethod, ZipWriter};

#[cfg(test)]
use crate::{
    app::models::hyperlink_artifact as hyperlink_artifact_model,
    entity::{hyperlink_artifact::HyperlinkArtifactKind, hyperlink_relation},
};

use crate::{
    app::{
        controllers::admin::{
            archive_transfer::{build_backup_zip, import_backup_zip, read_uploaded_backup_archive},
            artifact_maintenance::{
                MissingArtifactsSummary, build_artifact_settings_message,
                build_missing_artifacts_plan, build_missing_screenshot_hyperlink_ids,
                checkbox_checked, delete_og_artifacts_and_clear_fields,
                delete_readability_artifacts_and_clear_search, delete_screenshot_artifacts,
                delete_source_artifacts, enqueue_hyperlink_jobs, execute_missing_artifacts_plan,
            },
        },
        helpers::admin_dashboard::{
            ADMIN_TAB_ARTIFACTS_PATH, ADMIN_TAB_IMPORT_EXPORT_PATH,
            ADMIN_TAB_LLM_INTERACTIONS_PATH, ADMIN_TAB_OVERVIEW_PATH, ADMIN_TAB_QUEUE_PATH,
            ADMIN_TAB_STORAGE_PATH, AdminTab, format_bytes, format_bytes_f64,
            format_relative_duration, llm_interactions_href,
        },
        models::{
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
        hyperlink_artifact::{self},
        hyperlink_processing_job::{self, HyperlinkProcessingJobKind, HyperlinkProcessingJobState},
        llm_interaction,
    },
    integrations::mathpix::{MathpixStatus, MathpixUsageSummary},
    server::{
        admin_backup::{BackupDownloadError, BackupStatusResponse},
        admin_import::ImportStatusResponse,
        chromium_diagnostics::ChromiumDiagnostics,
        context::Context,
        flash::{Flash, FlashName, redirect_with_flash},
        font_diagnostics::FontDiagnostics,
    },
};

use crate::server::{
    admin_jobs::{fetch_pending_queue_counts, set_all_queued_rows_cleared},
    views,
};

#[cfg(test)]
use crate::app::controllers::admin::archive_transfer::{
    BACKUP_ARTIFACTS_PATH, BACKUP_HYPERLINKS_PATH, BACKUP_MANIFEST_PATH,
    BACKUP_READABILITY_PROGRESS_PATH, BACKUP_RELATIONS_PATH, BACKUP_VERSION, BackupManifest,
    HyperlinkArtifactBackupRow, HyperlinkBackupRow, HyperlinkRelationBackupRow,
    ReadabilityProgressBackupRow, read_zip_binary_file, read_zip_json_file,
    write_zip_binary_file_with_compression, write_zip_json_file,
};

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

async fn index(State(state): State<Context>, headers: HeaderMap) -> Response {
    render_admin_tab(AdminTab::Overview, &state, &headers, None).await
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
                Ok(summary) => {
                    backup_manager.mark_ready(job_id, summary);
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
                    import_manager.mark_ready(job_id, summary);
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

fn now_utc() -> sea_orm::entity::prelude::DateTime {
    sea_orm::entity::prelude::DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}

fn format_datetime(value: &sea_orm::entity::prelude::DateTime) -> String {
    value.format("%Y-%m-%dT%H:%M:%S%.fZ").to_string()
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

    fn total_artifact_storage_bytes(&self) -> u64 {
        (self.stats.saved_artifacts_size_bytes.max(0) as u64)
            .saturating_add(self.stats.discovered_artifacts_size_bytes.max(0) as u64)
    }

    fn total_storage_bytes(&self) -> u64 {
        self.stats
            .db_size_total_bytes
            .saturating_add(self.total_artifact_storage_bytes())
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
