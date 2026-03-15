use axum::Router;
use axum_test::multipart::{MultipartForm, Part};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    Statement,
};
use serde_json::json;
use std::{
    io::Cursor,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use zip::ZipArchive;

use super::*;
use crate::test_support;

fn default_mathpix_status() -> MathpixStatus {
    MathpixStatus {
        enabled: false,
        reason: "disabled: MATHPIX_API_TOKEN not set".to_string(),
    }
}

fn default_mathpix_usage_summary() -> MathpixUsageSummary {
    MathpixUsageSummary::default()
}

fn default_llm_settings() -> LlmSettings {
    LlmSettings::default()
}

#[test]
fn llm_models_endpoints_prioritizes_openai_candidates() {
    let endpoints =
        llm_models_endpoints("https://api.openai.com/v1").expect("endpoints should parse");
    let urls: Vec<String> = endpoints
        .iter()
        .map(|endpoint| endpoint.as_str().to_string())
        .collect();

    assert_eq!(
        urls,
        vec![
            "https://api.openai.com/v1/models".to_string(),
            "https://api.openai.com/v1/model/info".to_string(),
            "https://api.openai.com/models".to_string(),
            "https://api.openai.com/model/info".to_string(),
            "https://api.openai.com/api/tags".to_string()
        ]
    );
}

#[test]
fn llm_models_endpoints_rewrites_chat_completions_suffix() {
    let endpoints = llm_models_endpoints("https://api.openai.com/v1/chat/completions")
        .expect("endpoints should parse");
    let urls: Vec<String> = endpoints
        .iter()
        .map(|endpoint| endpoint.as_str().to_string())
        .collect();

    assert_eq!(
        urls,
        vec![
            "https://api.openai.com/v1/models".to_string(),
            "https://api.openai.com/v1/model/info".to_string(),
            "https://api.openai.com/models".to_string(),
            "https://api.openai.com/model/info".to_string(),
            "https://api.openai.com/api/tags".to_string()
        ]
    );
}

#[test]
fn llm_models_endpoints_prioritizes_ollama_candidates() {
    let endpoints = llm_models_endpoints("http://ollama:3000/api").expect("endpoints should parse");
    let urls: Vec<String> = endpoints
        .iter()
        .map(|endpoint| endpoint.as_str().to_string())
        .collect();

    assert_eq!(
        urls,
        vec![
            "http://ollama:3000/v1/models".to_string(),
            "http://ollama:3000/v1/model/info".to_string(),
            "http://ollama:3000/api/tags".to_string(),
            "http://ollama:3000/api/models".to_string(),
            "http://ollama:3000/api/model/info".to_string(),
            "http://ollama:3000/models".to_string(),
            "http://ollama:3000/model/info".to_string()
        ]
    );
}

#[test]
fn detects_backend_kind_from_models_endpoint_path() {
    assert_eq!(
        llm_backend_kind_for_models_path("/api/tags"),
        LlmBackendKind::Ollama
    );
    assert_eq!(
        llm_backend_kind_for_models_path("/v1/models"),
        LlmBackendKind::OpenAiCompatible
    );
    assert_eq!(
        llm_backend_kind_for_models_path("/v1/model/info"),
        LlmBackendKind::OpenAiCompatible
    );
}

#[test]
fn extract_llm_model_ids_reads_openai_data_list() {
    let payload = json!({
        "object": "list",
        "data": [
            { "id": "gpt-4o-mini" },
            { "id": "gpt-4.1-mini" }
        ]
    });
    let models = extract_llm_model_ids(&payload);
    assert_eq!(
        models,
        vec!["gpt-4.1-mini".to_string(), "gpt-4o-mini".to_string()]
    );
}

#[test]
fn extract_llm_model_ids_reads_models_array_variants() {
    let payload = json!({
        "models": [
            "llama3.2:3b",
            { "name": "qwen3:14b" },
            { "model": "mistral-small" }
        ]
    });
    let models = extract_llm_model_ids(&payload);
    assert_eq!(
        models,
        vec![
            "llama3.2:3b".to_string(),
            "mistral-small".to_string(),
            "qwen3:14b".to_string()
        ]
    );
}

#[test]
fn extract_llm_model_ids_reads_litellm_model_info_map() {
    let payload = json!({
        "model_info": {
            "openai/gpt-4o-mini": {
                "model_name": "openai/gpt-4o-mini"
            },
            "ollama/qwen3.5": {
                "litellm_params": {
                    "model": "ollama/qwen3.5"
                }
            }
        }
    });
    let models = extract_llm_model_ids(&payload);
    assert_eq!(
        models,
        vec![
            "ollama/qwen3.5".to_string(),
            "openai/gpt-4o-mini".to_string()
        ]
    );
}

#[test]
fn render_index_shows_chromium_setup_when_missing() {
    let summary = MissingArtifactsSummary::default();
    let stats = AdminDatasetStats::default();
    let chromium = ChromiumDiagnostics {
        chromium_path: "chromium".to_string(),
        chromium_resolved_path: None,
        chromium_found: false,
    };
    let fonts = FontDiagnostics::default();
    let mathpix = default_mathpix_status();
    let mathpix_usage = default_mathpix_usage_summary();

    let html = render_index(
        AdminTab::Overview,
        &summary,
        &stats,
        &ArtifactCollectionSettings::default(),
        &default_llm_settings(),
        AdminLlmInteractionsPage::default(),
        &crate::server::admin_jobs::QueuePendingCounts {
            pending: 0,
            queued: 0,
            processing: 0,
        },
        &chromium,
        &fonts,
        &mathpix,
        &mathpix_usage,
    )
    .expect("admin template should render");
    assert!(html.contains("Screenshot browser setup required"));
    assert!(html.contains("CHROMIUM_PATH"));
}

#[test]
fn render_index_hides_chromium_setup_when_available() {
    let summary = MissingArtifactsSummary::default();
    let stats = AdminDatasetStats::default();
    let chromium = ChromiumDiagnostics {
        chromium_path: "/usr/bin/chromium".to_string(),
        chromium_resolved_path: Some("/usr/bin/chromium".to_string()),
        chromium_found: true,
    };
    let fonts = FontDiagnostics::default();
    let mathpix = default_mathpix_status();
    let mathpix_usage = default_mathpix_usage_summary();

    let html = render_index(
        AdminTab::Overview,
        &summary,
        &stats,
        &ArtifactCollectionSettings::default(),
        &default_llm_settings(),
        AdminLlmInteractionsPage::default(),
        &crate::server::admin_jobs::QueuePendingCounts {
            pending: 0,
            queued: 0,
            processing: 0,
        },
        &chromium,
        &fonts,
        &mathpix,
        &mathpix_usage,
    )
    .expect("admin template should render");
    assert!(!html.contains("Screenshot browser setup required"));
}

#[test]
fn render_index_shows_font_setup_when_missing_fonts_detected() {
    let summary = MissingArtifactsSummary::default();
    let stats = AdminDatasetStats::default();
    let chromium = ChromiumDiagnostics {
        chromium_path: "/usr/bin/chromium".to_string(),
        chromium_resolved_path: Some("/usr/bin/chromium".to_string()),
        chromium_found: true,
    };
    let fonts = FontDiagnostics {
        checks_enabled: true,
        applicable: true,
        platform: "linux".to_string(),
        fontconfig_found: true,
        required_families: vec![
            "Noto Sans".to_string(),
            "Noto Serif".to_string(),
            "Noto Sans Mono".to_string(),
            "Noto Color Emoji".to_string(),
        ],
        missing_families: vec!["Noto Color Emoji".to_string()],
        resolved_matches: Vec::new(),
        install_hint: Some(
            "apt install -y fontconfig fonts-noto fonts-noto-cjk fonts-noto-color-emoji"
                .to_string(),
        ),
        fontconfig_error: None,
    };
    let mathpix = default_mathpix_status();
    let mathpix_usage = default_mathpix_usage_summary();

    let html = render_index(
        AdminTab::Overview,
        &summary,
        &stats,
        &ArtifactCollectionSettings::default(),
        &default_llm_settings(),
        AdminLlmInteractionsPage::default(),
        &crate::server::admin_jobs::QueuePendingCounts {
            pending: 0,
            queued: 0,
            processing: 0,
        },
        &chromium,
        &fonts,
        &mathpix,
        &mathpix_usage,
    )
    .expect("admin template should render");
    assert!(html.contains("Screenshot font setup recommended"));
    assert!(html.contains("Noto Color Emoji"));
    assert!(html.contains("fontconfig"));
}

#[test]
fn render_index_shows_mathpix_enabled_status() {
    let summary = MissingArtifactsSummary::default();
    let stats = AdminDatasetStats::default();
    let chromium = ChromiumDiagnostics {
        chromium_path: "/usr/bin/chromium".to_string(),
        chromium_resolved_path: Some("/usr/bin/chromium".to_string()),
        chromium_found: true,
    };
    let fonts = FontDiagnostics::default();
    let mathpix = MathpixStatus {
        enabled: true,
        reason: "enabled".to_string(),
    };
    let mathpix_usage = default_mathpix_usage_summary();

    let html = render_index(
        AdminTab::Overview,
        &summary,
        &stats,
        &ArtifactCollectionSettings::default(),
        &default_llm_settings(),
        AdminLlmInteractionsPage::default(),
        &crate::server::admin_jobs::QueuePendingCounts {
            pending: 0,
            queued: 0,
            processing: 0,
        },
        &chromium,
        &fonts,
        &mathpix,
        &mathpix_usage,
    )
    .expect("admin template should render");
    assert!(html.contains("PDF Mathpix parsing"));
    assert!(html.contains("Enabled"));
}

#[test]
fn render_index_shows_mathpix_disabled_reason() {
    let summary = MissingArtifactsSummary::default();
    let stats = AdminDatasetStats::default();
    let chromium = ChromiumDiagnostics {
        chromium_path: "/usr/bin/chromium".to_string(),
        chromium_resolved_path: Some("/usr/bin/chromium".to_string()),
        chromium_found: true,
    };
    let fonts = FontDiagnostics::default();
    let mathpix = MathpixStatus {
        enabled: false,
        reason: "disabled: MATHPIX_APP_ID not set".to_string(),
    };
    let mathpix_usage = default_mathpix_usage_summary();

    let html = render_index(
        AdminTab::Overview,
        &summary,
        &stats,
        &ArtifactCollectionSettings::default(),
        &default_llm_settings(),
        AdminLlmInteractionsPage::default(),
        &crate::server::admin_jobs::QueuePendingCounts {
            pending: 0,
            queued: 0,
            processing: 0,
        },
        &chromium,
        &fonts,
        &mathpix,
        &mathpix_usage,
    )
    .expect("admin template should render");
    assert!(html.contains("PDF Mathpix parsing"));
    assert!(html.contains("Disabled"));
    assert!(html.contains("MATHPIX_APP_ID"));
}

#[test]
fn render_index_shows_mathpix_usage_summary() {
    let summary = MissingArtifactsSummary::default();
    let stats = AdminDatasetStats::default();
    let chromium = ChromiumDiagnostics {
        chromium_path: "/usr/bin/chromium".to_string(),
        chromium_resolved_path: Some("/usr/bin/chromium".to_string()),
        chromium_found: true,
    };
    let fonts = FontDiagnostics::default();
    let mathpix = MathpixStatus {
        enabled: true,
        reason: "enabled".to_string(),
    };
    let mathpix_usage = MathpixUsageSummary {
        month: crate::integrations::mathpix::MathpixUsageWindow {
            total_requests: 12,
            estimated_cost_usd: 0.09,
            breakdown: vec![
                crate::integrations::mathpix::MathpixUsageBreakdown {
                    usage_type: "image-async".to_string(),
                    count: 9,
                    cost_class: crate::integrations::mathpix::MathpixUsageCostClass::ImageRequest,
                    estimated_cost_usd: 0.02,
                },
                crate::integrations::mathpix::MathpixUsageBreakdown {
                    usage_type: "pdf-async".to_string(),
                    count: 3,
                    cost_class: crate::integrations::mathpix::MathpixUsageCostClass::PdfRequest,
                    estimated_cost_usd: 0.07,
                },
            ],
        },
        all_time: crate::integrations::mathpix::MathpixUsageWindow {
            total_requests: 98,
            estimated_cost_usd: 0.74,
            breakdown: vec![
                crate::integrations::mathpix::MathpixUsageBreakdown {
                    usage_type: "image".to_string(),
                    count: 65,
                    cost_class: crate::integrations::mathpix::MathpixUsageCostClass::ImageRequest,
                    estimated_cost_usd: 0.13,
                },
                crate::integrations::mathpix::MathpixUsageBreakdown {
                    usage_type: "pdf-async".to_string(),
                    count: 33,
                    cost_class: crate::integrations::mathpix::MathpixUsageCostClass::PdfRequest,
                    estimated_cost_usd: 0.61,
                },
            ],
        },
        warning: None,
    };

    let html = render_index(
        AdminTab::Overview,
        &summary,
        &stats,
        &ArtifactCollectionSettings::default(),
        &default_llm_settings(),
        AdminLlmInteractionsPage::default(),
        &crate::server::admin_jobs::QueuePendingCounts {
            pending: 0,
            queued: 0,
            processing: 0,
        },
        &chromium,
        &fonts,
        &mathpix,
        &mathpix_usage,
    )
    .expect("admin template should render");
    assert!(
        html.contains("Current month: <code class=\"font-mono text-[0.9em]\">12 requests</code>")
    );
    assert!(html.contains("estimated <code class=\"font-mono text-[0.9em]\">$0.09</code>"));
    assert!(html.contains("All-time: <code class=\"font-mono text-[0.9em]\">98 requests</code>"));
    assert!(html.contains("estimated <code class=\"font-mono text-[0.9em]\">$0.74</code>"));
    assert!(html.contains("Current month usage breakdown"));
    assert!(html.contains("All-time usage breakdown"));
    assert!(html.contains("<code class=\"font-mono text-[0.9em]\">image-async</code>"));
    assert!(html.contains("<code class=\"font-mono text-[0.9em]\">pdf-async</code>"));
    assert!(html.contains("<code class=\"font-mono text-[0.9em]\">image</code>"));
    assert!(html.contains("<code class=\"font-mono text-[0.9em]\">pdf</code>"));
}

#[test]
fn render_index_shows_mathpix_usage_warning() {
    let summary = MissingArtifactsSummary::default();
    let stats = AdminDatasetStats::default();
    let chromium = ChromiumDiagnostics {
        chromium_path: "/usr/bin/chromium".to_string(),
        chromium_resolved_path: Some("/usr/bin/chromium".to_string()),
        chromium_found: true,
    };
    let fonts = FontDiagnostics::default();
    let mathpix = MathpixStatus {
        enabled: true,
        reason: "enabled".to_string(),
    };
    let mathpix_usage = MathpixUsageSummary {
        warning: Some("Mathpix usage unavailable: timeout".to_string()),
        ..Default::default()
    };

    let html = render_index(
        AdminTab::Overview,
        &summary,
        &stats,
        &ArtifactCollectionSettings::default(),
        &default_llm_settings(),
        AdminLlmInteractionsPage::default(),
        &crate::server::admin_jobs::QueuePendingCounts {
            pending: 0,
            queued: 0,
            processing: 0,
        },
        &chromium,
        &fonts,
        &mathpix,
        &mathpix_usage,
    )
    .expect("admin template should render");
    assert!(html.contains("Mathpix usage unavailable: timeout"));
}

#[tokio::test]
async fn artifact_settings_disable_does_not_delete_without_explicit_checkbox() {
    let (server, connection) = new_server(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, og_title, og_description, og_type, og_url, og_image_url, og_site_name, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example', 'https://example.com', 'https://example.com', 'OG Title', 'OG Description', 'article', 'https://example.com/post', 'https://example.com/image.png', 'Example', 0, 0, NULL, '2026-02-22 00:00:00', '2026-02-22 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES (1, 1, NULL, 'og_meta', X'7B7D', 'application/json', 2, '2026-02-22 00:01:00');
            "#,
        )
        .await;

    let response = server
        .post("/admin/artifact-settings")
        .text(artifact_settings_disable_og_form(false))
        .content_type("application/x-www-form-urlencoded")
        .await;
    response.assert_status_see_other();
    response.assert_header("location", "/admin/artifacts");

    let og_artifact_count = hyperlink_artifact::Entity::find()
        .filter(hyperlink_artifact::Column::Kind.eq(HyperlinkArtifactKind::OgMeta))
        .count(&connection)
        .await
        .expect("og artifact count should load");
    assert_eq!(og_artifact_count, 1);
}

#[tokio::test]
async fn artifact_settings_disable_with_delete_checkbox_removes_og_artifacts() {
    let (server, connection) = new_server(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, og_title, og_description, og_type, og_url, og_image_url, og_site_name, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example', 'https://example.com', 'https://example.com', 'OG Title', 'OG Description', 'article', 'https://example.com/post', 'https://example.com/image.png', 'Example', 0, 0, NULL, '2026-02-22 00:00:00', '2026-02-22 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 1, NULL, 'og_meta', X'7B7D', 'application/json', 2, '2026-02-22 00:01:00'),
                    (2, 1, NULL, 'og_image', X'89504E47', 'image/png', 4, '2026-02-22 00:01:00');
            "#,
        )
        .await;

    let response = server
        .post("/admin/artifact-settings")
        .text(artifact_settings_disable_og_form(true))
        .content_type("application/x-www-form-urlencoded")
        .await;
    response.assert_status_see_other();
    response.assert_header("location", "/admin/artifacts");

    let og_artifact_count = hyperlink_artifact::Entity::find()
        .filter(hyperlink_artifact::Column::Kind.is_in([
            HyperlinkArtifactKind::OgMeta,
            HyperlinkArtifactKind::OgImage,
            HyperlinkArtifactKind::OgError,
        ]))
        .count(&connection)
        .await
        .expect("og artifact count should load");
    assert_eq!(og_artifact_count, 0);

    let updated_link = hyperlink::Entity::find_by_id(1)
        .one(&connection)
        .await
        .expect("hyperlink lookup should succeed")
        .expect("hyperlink should exist");
    assert!(updated_link.og_title.is_none());
    assert!(updated_link.og_description.is_none());
    assert!(updated_link.og_type.is_none());
    assert!(updated_link.og_url.is_none());
    assert!(updated_link.og_image_url.is_none());
    assert!(updated_link.og_site_name.is_none());
}

fn artifact_settings_disable_og_form(with_delete: bool) -> String {
    let mut form =
        "collect_source=1&collect_screenshots=1&collect_screenshot_dark=1&collect_readability=1"
            .to_string();
    if with_delete {
        form.push_str("&delete_og_on_disable=1");
    }
    form
}

async fn new_server(seed_sql: &str) -> (axum_test::TestServer, sea_orm::DatabaseConnection) {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_schema(&connection).await;
    test_support::initialize_queue_jobs_schema(&connection).await;
    test_support::execute_sql(&connection, seed_sql).await;

    let app = Router::<Context>::new()
        .merge(routes())
        .with_state(Context {
            connection: connection.clone(),
            processing_queue: None,
            backup_exports: crate::server::admin_backup::AdminBackupManager::default(),
            backup_imports: crate::server::admin_import::AdminImportManager::default(),
        });
    (
        axum_test::TestServer::new(app).expect("test server should initialize"),
        connection,
    )
}

async fn wait_for_backup_ready(server: &axum_test::TestServer) -> AdminStatusResponse {
    for _ in 0..200 {
        let status = server.get("/admin/status").await;
        status.assert_status_ok();
        let payload: AdminStatusResponse = status.json();
        if payload.backup.state == "ready" {
            return payload;
        }
        assert_ne!(payload.backup.state, "failed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    panic!("timed out waiting for backup export to become ready");
}

async fn wait_for_import_ready(server: &axum_test::TestServer) -> AdminStatusResponse {
    for _ in 0..300 {
        let status = server.get("/admin/status").await;
        status.assert_status_ok();
        let payload: AdminStatusResponse = status.json();
        if payload.import.state == "ready" {
            return payload;
        }
        assert_ne!(payload.import.state, "failed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    panic!("timed out waiting for backup import to become ready");
}

async fn wait_for_import_running(server: &axum_test::TestServer) -> AdminStatusResponse {
    for _ in 0..300 {
        let status = server.get("/admin/status").await;
        status.assert_status_ok();
        let payload: AdminStatusResponse = status.json();
        if payload.import.state == "running" {
            return payload;
        }
        if payload.import.state == "failed" {
            panic!(
                "import failed before cancel request: {}",
                payload
                    .import
                    .error
                    .unwrap_or_else(|| "unknown error".to_string())
            );
        }
        if payload.import.state == "ready" {
            panic!("import completed before cancel request");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    panic!("timed out waiting for backup import to start running");
}

async fn wait_for_import_terminal(server: &axum_test::TestServer) -> AdminStatusResponse {
    for _ in 0..300 {
        let status = server.get("/admin/status").await;
        status.assert_status_ok();
        let payload: AdminStatusResponse = status.json();
        if payload.import.state != "running" {
            return payload;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    panic!("timed out waiting for backup import to leave running state");
}

async fn insert_queue_job(
    connection: &sea_orm::DatabaseConnection,
    id: i64,
    status: &str,
    payload: &str,
) {
    let statement = Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO jobs (
                id, job_type, payload, status, attempts, max_attempts, available_at, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"
                .to_string(),
            vec![
                id.into(),
                std::any::type_name::<crate::queue::ProcessingTask>().into(),
                payload.into(),
                status.into(),
                0.into(),
                20.into(),
                1.into(),
                1.into(),
                1.into(),
            ],
        );
    connection
        .execute(statement)
        .await
        .expect("queue row should insert");
}

async fn queue_status(connection: &sea_orm::DatabaseConnection, id: i64) -> String {
    let statement = Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "SELECT status FROM jobs WHERE id = ?".to_string(),
        vec![id.into()],
    );
    let row = connection
        .query_one(statement)
        .await
        .expect("queue lookup should succeed")
        .expect("queue row should exist");
    row.try_get_by_index::<String>(0)
        .expect("status should decode")
}

#[tokio::test]
async fn process_missing_artifacts_enqueues_snapshot_og_and_readability() {
    let (server, connection) = new_server(
            r#"
                INSERT INTO hyperlink (id, title, url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'No Artifacts', 'https://example.com/1', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00'),
                    (2, 'Source Only', 'https://example.com/2', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00'),
                    (3, 'Complete', 'https://example.com/3', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 2, NULL, 'snapshot_warc', X'77617263', 'application/warc', 4, '2026-02-21 00:01:00'),
                    (2, 3, NULL, 'snapshot_warc', X'77617263', 'application/warc', 4, '2026-02-21 00:01:00'),
                    (3, 3, NULL, 'og_meta', X'7B7D', 'application/json', 2, '2026-02-21 00:01:00'),
                    (4, 3, NULL, 'readable_text', X'74657874', 'text/markdown; charset=utf-8', 4, '2026-02-21 00:01:00'),
                    (5, 3, NULL, 'readable_meta', X'7B7D', 'application/json', 2, '2026-02-21 00:01:00');
            "#,
        )
        .await;

    let action = server.post("/admin/process-missing-artifacts").await;
    action.assert_status_see_other();
    action.assert_header("location", "/admin/artifacts");

    let snapshot_jobs = hyperlink_processing_job::Entity::find()
        .filter(hyperlink_processing_job::Column::Kind.eq(HyperlinkProcessingJobKind::Snapshot))
        .count(&connection)
        .await
        .expect("snapshot jobs count should succeed");
    assert_eq!(snapshot_jobs, 1);

    let og_jobs = hyperlink_processing_job::Entity::find()
        .filter(hyperlink_processing_job::Column::Kind.eq(HyperlinkProcessingJobKind::Og))
        .count(&connection)
        .await
        .expect("og jobs count should succeed");
    assert_eq!(og_jobs, 1);

    let readability_jobs = hyperlink_processing_job::Entity::find()
        .filter(hyperlink_processing_job::Column::Kind.eq(HyperlinkProcessingJobKind::Readability))
        .count(&connection)
        .await
        .expect("readability jobs count should succeed");
    assert_eq!(readability_jobs, 1);
}

#[tokio::test]
async fn admin_page_shows_missing_artifact_summary() {
    let (server, _) = new_server(
            r#"
                INSERT INTO hyperlink (id, title, url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Missing Source', 'https://example.com/1', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00'),
                    (2, 'Missing Readability', 'https://example.com/2', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 2, NULL, 'snapshot_warc', X'77617263', 'application/warc', 4, '2026-02-21 00:01:00');
            "#,
        )
        .await;

    let artifacts = server.get("/admin/artifacts").await;
    artifacts.assert_status_ok();
    let artifacts_body = artifacts.text();
    assert!(artifacts_body.contains("Process all artifacts"));
    assert!(artifacts_body.contains("data-confirm=\"Process all artifacts?"));
    assert!(artifacts_body.contains("Missing source"));
    assert!(artifacts_body.contains("Missing Open Graph"));
    assert!(artifacts_body.contains("Missing readability"));
    assert!(artifacts_body.contains("Snapshot to queue"));
    assert!(artifacts_body.contains("Open Graph to queue"));
    assert!(artifacts_body.contains("Readability to queue"));
    assert!(artifacts_body.contains("href=\"/admin/overview\""));
    assert!(artifacts_body.contains("href=\"/admin/artifacts\""));
    assert!(artifacts_body.contains("href=\"/admin/llm-interactions\""));
    assert!(artifacts_body.contains("href=\"/admin/queue\""));
    assert!(artifacts_body.contains("href=\"/admin/import-export\""));
    assert!(artifacts_body.contains("href=\"/admin/storage\""));
    assert!(artifacts_body.contains("data-queue-pending-badge"));
    assert!(!artifacts_body.contains("Queue controls"));
    assert!(!artifacts_body.contains("data-admin-backup"));
    assert!(!artifacts_body.contains("data-admin-version"));

    let queue = server.get("/admin/queue").await;
    queue.assert_status_ok();
    let queue_body = queue.text();
    assert!(queue_body.contains("Pending queue rows"));
    assert!(queue_body.contains("data-admin-queue-pending>0<"));
    assert!(queue_body.contains("data-admin-queue-queued>0<"));
    assert!(queue_body.contains("data-admin-queue-processing>0<"));
    assert!(queue_body.contains("href=\"/admin/jobs?status=queued\""));
    assert!(queue_body.contains("href=\"/admin/jobs?status=processing\""));
    assert!(queue_body.contains("href=\"/admin/jobs\""));
    assert!(queue_body.contains("Queue controls"));
    assert!(queue_body.contains("action=\"/admin/clear-queue\""));
    assert!(queue_body.contains("action=\"/admin/pause-queue\""));
    assert!(queue_body.contains("action=\"/admin/resume-queue\""));
    assert!(!queue_body.contains("Process all artifacts"));

    let llm = server.get("/admin/llm-interactions").await;
    llm.assert_status_ok();
    let llm_body = llm.text();
    assert!(llm_body.contains("LLM settings"));
    assert!(llm_body.contains("data-llm-settings-form"));
    assert!(llm_body.contains("data-llm-check-button"));
    assert!(llm_body.contains("LLM interactions"));
    assert!(llm_body.contains("action=\"/admin/clear-llm-interactions\""));
    assert!(llm_body.contains("Clear interactions"));
    assert!(llm_body.contains("Nothing to clear."));
    assert!(!llm_body.contains("Process all artifacts"));

    let import_export = server.get("/admin/import-export").await;
    import_export.assert_status_ok();
    let import_export_body = import_export.text();
    assert!(import_export_body.contains("data-admin-backup"));
    assert!(import_export_body.contains("data-admin-backup-create"));
    assert!(import_export_body.contains("data-admin-backup-cancel"));
    assert!(import_export_body.contains("data-admin-backup-download"));
    assert!(import_export_body.contains("data-admin-import"));
    assert!(import_export_body.contains("data-admin-import-form"));
    assert!(import_export_body.contains("data-admin-import-submit"));
    assert!(import_export_body.contains("data-admin-import-status"));

    let overview = server.get("/admin/overview").await;
    overview.assert_status_ok();
    let overview_body = overview.text();
    assert!(overview_body.contains("data-admin-version"));
    assert!(overview_body.contains(APP_VERSION));
    assert!(!overview_body.contains("Process all artifacts"));
}

#[tokio::test]
async fn queue_tab_shows_pending_counts_from_processing_task_rows() {
    let (server, connection) = new_server("").await;
    insert_queue_job(&connection, 1, "queued", r#"{"processing_job_id":1}"#).await;
    insert_queue_job(&connection, 2, "processing", r#"{"processing_job_id":2}"#).await;
    insert_queue_job(&connection, 3, "failed", r#"{"processing_job_id":3}"#).await;
    insert_queue_job(&connection, 4, "completed", r#"{"processing_job_id":4}"#).await;

    let queue = server.get("/admin/queue").await;
    queue.assert_status_ok();
    let body = queue.text();

    assert!(body.contains("data-admin-queue-pending>2<"));
    assert!(body.contains("data-admin-queue-queued>1<"));
    assert!(body.contains("data-admin-queue-processing>1<"));
    assert!(body.contains("href=\"/admin/jobs?status=queued\""));
    assert!(body.contains("href=\"/admin/jobs?status=processing\""));
    assert!(body.contains("href=\"/admin/jobs\""));
}

#[tokio::test]
async fn clear_queue_marks_only_queued_rows_cleared() {
    let (server, connection) = new_server("").await;
    insert_queue_job(&connection, 1, "queued", r#"{"processing_job_id":1}"#).await;
    insert_queue_job(&connection, 2, "processing", r#"{"processing_job_id":2}"#).await;
    insert_queue_job(&connection, 3, "failed", r#"{"processing_job_id":3}"#).await;
    insert_queue_job(&connection, 4, "completed", r#"{"processing_job_id":4}"#).await;

    let action = server.post("/admin/clear-queue").await;
    action.assert_status_see_other();
    action.assert_header("location", "/admin/queue");

    assert_eq!(queue_status(&connection, 1).await, "cleared");
    assert_eq!(queue_status(&connection, 2).await, "processing");
    assert_eq!(queue_status(&connection, 3).await, "failed");
    assert_eq!(queue_status(&connection, 4).await, "completed");
}

#[tokio::test]
async fn clear_llm_interactions_deletes_all_rows_and_redirects() {
    let (server, connection) = new_server("").await;

    for index in 0..2 {
        llm_interaction_model::record(
            &connection,
            llm_interaction_model::NewLlmInteraction {
                kind: format!("kind-{index}"),
                provider: "openai_compatible".to_string(),
                model: "gpt-4.1-mini".to_string(),
                endpoint_url: "https://example.com/v1/chat/completions".to_string(),
                api_kind: "openai_compatible".to_string(),
                request_body: "{}".to_string(),
                ..Default::default()
            },
        )
        .await
        .expect("interaction should save");
    }

    let action = server.post("/admin/clear-llm-interactions").await;
    action.assert_status_see_other();
    action.assert_header("location", "/admin/llm-interactions");

    let count = llm_interaction::Entity::find()
        .count(&connection)
        .await
        .expect("llm interactions count should succeed");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn pause_queue_redirects_when_queue_controls_are_unavailable() {
    let (server, _) = new_server("").await;

    let action = server.post("/admin/pause-queue").await;
    action.assert_status_see_other();
    action.assert_header("location", "/admin/queue");
}

#[tokio::test]
async fn resume_queue_redirects_when_queue_controls_are_unavailable() {
    let (server, _) = new_server("").await;

    let action = server.post("/admin/resume-queue").await;
    action.assert_status_see_other();
    action.assert_header("location", "/admin/queue");
}

#[tokio::test]
async fn process_missing_artifacts_skips_when_active_jobs_exist() {
    let (server, _) = new_server(
            r#"
                INSERT INTO hyperlink (id, title, url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Missing Source', 'https://example.com/1', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00'),
                    (2, 'Missing Readability', 'https://example.com/2', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 2, NULL, 'snapshot_warc', X'77617263', 'application/warc', 4, '2026-02-21 00:01:00');
                INSERT INTO hyperlink_processing_job (id, hyperlink_id, kind, state, error_message, queued_at, started_at, finished_at, created_at, updated_at)
                VALUES
                    (10, 1, 'snapshot', 'queued', NULL, '2026-02-21 00:02:00', NULL, NULL, '2026-02-21 00:02:00', '2026-02-21 00:02:00'),
                    (11, 2, 'readability', 'running', NULL, '2026-02-21 00:02:00', '2026-02-21 00:02:30', NULL, '2026-02-21 00:02:00', '2026-02-21 00:02:30');
            "#,
        )
        .await;

    let action = server.post("/admin/process-missing-artifacts").await;
    action.assert_status_see_other();
    action.assert_header("location", "/admin/artifacts");
}

#[tokio::test]
async fn admin_page_disables_process_button_when_no_work_is_needed() {
    let (server, _) = new_server(
            r#"
                INSERT INTO hyperlink (id, title, url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Complete', 'https://example.com/1', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 1, NULL, 'snapshot_warc', X'77617263', 'application/warc', 4, '2026-02-21 00:01:00'),
                    (2, 1, NULL, 'og_meta', X'7B7D', 'application/json', 2, '2026-02-21 00:01:00'),
                    (3, 1, NULL, 'readable_text', X'74657874', 'text/markdown; charset=utf-8', 4, '2026-02-21 00:01:00'),
                    (4, 1, NULL, 'readable_meta', X'7B7D', 'application/json', 2, '2026-02-21 00:01:00');
            "#,
        )
        .await;

    let page = server.get("/admin/artifacts").await;
    page.assert_status_ok();
    let body = page.text();
    assert!(body.contains("Process all artifacts</button>"));
    assert!(body.contains("disabled>Process all artifacts</button>"));
}

#[tokio::test]
async fn admin_page_shows_flash_style_examples() {
    let (server, _) = new_server(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Root', 'https://example.com/root', 'https://example.com/root', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00'),
                    (2, 'Child', 'https://example.com/child', 'https://example.com/child', 1, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 1, NULL, 'snapshot_warc', X'77617263', 'application/warc', 4, '2026-02-21 00:01:00');
                INSERT INTO hyperlink_processing_job (id, hyperlink_id, kind, state, error_message, queued_at, started_at, finished_at, created_at, updated_at)
                VALUES
                    (1, 1, 'snapshot', 'queued', NULL, '2026-02-21 00:02:00', NULL, NULL, '2026-02-21 00:02:00', '2026-02-21 00:02:00'),
                    (2, 2, 'readability', 'succeeded', NULL, '2026-02-21 00:02:00', '2026-02-21 00:02:30', '2026-02-21 00:03:00', '2026-02-21 00:02:00', '2026-02-21 00:03:00');
            "#,
        )
        .await;

    let overview = server.get("/admin/overview").await;
    overview.assert_status_ok();
    let overview_body = overview.text();
    assert!(overview_body.contains("Diagnostics and examples"));
    assert!(overview_body.contains("Dataset stats"));
    assert!(overview_body.contains("Flash examples"));
    assert!(overview_body.contains("border-notice-border"));
    assert!(overview_body.contains("border-invalid"));
    assert!(overview_body.contains("border-dev-alert-border"));
    assert!(overview_body.contains("Root links"));
    assert!(overview_body.contains("Discovered links"));
    assert!(overview_body.contains("Active jobs"));
    assert!(!overview_body.contains("Storage utilization"));

    let storage = server.get("/admin/storage").await;
    storage.assert_status_ok();
    let storage_body = storage.text();
    assert!(storage_body.contains("Storage utilization"));
    assert!(storage_body.contains("DB size"));
    assert!(storage_body.contains("Saved artifacts size"));
    assert!(storage_body.contains("Discovered artifacts size"));
    assert!(storage_body.contains("avg"));
    assert!(storage_body.contains("Artifact storage by type"));
    assert!(storage_body.contains("snapshot_warc"));
}

#[tokio::test]
async fn build_dataset_stats_splits_artifact_size_bytes_and_sorts_kinds_by_total_desc() {
    let (_, connection) = new_server(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Root', 'https://example.com/root', 'https://example.com/root', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00'),
                    (2, 'Discovered', 'https://example.com/discovered', 'https://example.com/discovered', 1, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 1, NULL, 'snapshot_warc', X'00', 'application/warc', 10, '2026-02-21 00:01:00'),
                    (2, 2, NULL, 'snapshot_warc', X'00', 'application/warc', 15, '2026-02-21 00:01:00'),
                    (3, 1, NULL, 'og_meta', X'7B7D', 'application/json', 20, '2026-02-21 00:01:00'),
                    (4, 2, NULL, 'og_meta', X'7B7D', 'application/json', 25, '2026-02-21 00:01:00');
            "#,
        )
        .await;

    let stats = build_dataset_stats(&connection)
        .await
        .expect("dataset stats should load");

    assert_eq!(stats.saved_artifacts_size_bytes, 30);
    assert_eq!(stats.saved_artifacts_count, 2);
    assert_eq!(stats.discovered_artifacts_size_bytes, 40);
    assert_eq!(stats.discovered_artifacts_count, 2);
    assert_eq!(stats.db_size_total_bytes, 0);
    assert_eq!(stats.artifact_storage_by_kind.len(), 2);
    assert_eq!(stats.artifact_storage_by_kind[0].kind, "og_meta");
    assert_eq!(stats.artifact_storage_by_kind[0].saved_size_bytes, 20);
    assert_eq!(stats.artifact_storage_by_kind[0].saved_artifact_count, 1);
    assert_eq!(stats.artifact_storage_by_kind[0].discovered_size_bytes, 25);
    assert_eq!(
        stats.artifact_storage_by_kind[0].discovered_artifact_count,
        1
    );
    assert_eq!(stats.artifact_storage_by_kind[1].kind, "snapshot_warc");
    assert_eq!(stats.artifact_storage_by_kind[1].saved_size_bytes, 10);
    assert_eq!(stats.artifact_storage_by_kind[1].saved_artifact_count, 1);
    assert_eq!(stats.artifact_storage_by_kind[1].discovered_size_bytes, 15);
    assert_eq!(
        stats.artifact_storage_by_kind[1].discovered_artifact_count,
        1
    );
}

#[test]
fn sqlite_disk_stats_from_main_path_counts_main_wal_and_shm() {
    let uniq = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after unix epoch")
        .as_nanos();
    let main_path = std::env::temp_dir().join(format!(
        "hyperlinked-admin-disk-stats-{}-{}.db",
        std::process::id(),
        uniq
    ));
    let wal_path = append_path_suffix(&main_path, "-wal");
    let shm_path = append_path_suffix(&main_path, "-shm");

    std::fs::write(&main_path, vec![0u8; 11]).expect("main db file should be created");
    std::fs::write(&wal_path, vec![0u8; 7]).expect("wal file should be created");
    std::fs::write(&shm_path, vec![0u8; 3]).expect("shm file should be created");

    let stats = sqlite_disk_stats_from_main_path(&main_path)
        .expect("disk stats should load from filesystem");

    assert_eq!(stats.main_bytes, 11);
    assert_eq!(stats.wal_bytes, 7);
    assert_eq!(stats.shm_bytes, 3);
    assert_eq!(stats.total_bytes(), 21);

    let _ = std::fs::remove_file(&main_path);
    let _ = std::fs::remove_file(&wal_path);
    let _ = std::fs::remove_file(&shm_path);
}

#[tokio::test]
async fn admin_export_downloads_zip_payload_with_artifacts() {
    let (server, _) = new_server(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Example export', 'https://example.com/canonical', 'https://example.com/raw?utm_source=test', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00'),
                    (2, 'Discovered child', 'https://example.com/child', 'https://example.com/child', 1, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00');
                INSERT INTO hyperlink_relation (id, parent_hyperlink_id, child_hyperlink_id, created_at)
                VALUES
                    (3, 1, 2, '2026-02-21 00:00:30');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (9, 1, NULL, 'screenshot_webp', X'52494646', 'image/webp', 4, '2026-02-21 00:01:00');
            "#,
        )
        .await;

    let not_ready = server.get("/admin/export/download").await;
    not_ready.assert_status(StatusCode::CONFLICT);

    let start = server.post("/admin/export/start").await;
    start.assert_status(StatusCode::ACCEPTED);

    let status_payload = wait_for_backup_ready(&server).await;
    assert_eq!(status_payload.backup.state, "ready");
    assert!(status_payload.backup.download_ready);
    assert_eq!(status_payload.backup.hyperlinks, Some(2));
    assert_eq!(status_payload.backup.relations, Some(1));
    assert_eq!(status_payload.backup.artifacts, Some(1));

    let export = server.get("/admin/export/download").await;
    export.assert_status_ok();
    export.assert_header("content-type", "application/zip");
    export.assert_header(
        "content-disposition",
        "attachment; filename=\"hyperlinked-backup.zip\"",
    );

    let mut archive = ZipArchive::new(Cursor::new(export.as_bytes().to_vec()))
        .expect("export should be a valid zip archive");
    let manifest_entry = archive
        .by_name(BACKUP_MANIFEST_PATH)
        .expect("manifest should exist");
    assert_eq!(manifest_entry.compression(), CompressionMethod::Deflated);
    drop(manifest_entry);

    let artifact_entry = archive
        .by_name("artifacts/9.bin")
        .expect("artifact payload should exist");
    assert_eq!(artifact_entry.compression(), CompressionMethod::Deflated);
    drop(artifact_entry);

    let manifest: BackupManifest =
        read_zip_json_file(&mut archive, BACKUP_MANIFEST_PATH).expect("manifest should parse");
    assert_eq!(manifest.version, BACKUP_VERSION);
    assert_eq!(manifest.hyperlinks, 2);
    assert_eq!(manifest.relations, 1);
    assert_eq!(manifest.artifacts, 1);

    let hyperlinks: Vec<HyperlinkBackupRow> =
        read_zip_json_file(&mut archive, BACKUP_HYPERLINKS_PATH).expect("hyperlinks should parse");
    assert_eq!(hyperlinks.len(), 2);
    assert_eq!(hyperlinks[0].title, "Example export");
    assert_eq!(
        hyperlinks[0].raw_url,
        "https://example.com/raw?utm_source=test"
    );

    let relations: Vec<HyperlinkRelationBackupRow> =
        read_zip_json_file(&mut archive, BACKUP_RELATIONS_PATH).expect("relations should parse");
    assert_eq!(relations.len(), 1);
    assert_eq!(relations[0].parent_hyperlink_id, 1);
    assert_eq!(relations[0].child_hyperlink_id, 2);

    let artifacts: Vec<HyperlinkArtifactBackupRow> =
        read_zip_json_file(&mut archive, BACKUP_ARTIFACTS_PATH).expect("artifacts should parse");
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].id, 9);
    assert_eq!(artifacts[0].payload_path, "artifacts/9.bin");

    let payload = read_zip_binary_file(&mut archive, "artifacts/9.bin")
        .expect("artifact payload should exist");
    assert_eq!(payload, vec![0x52, 0x49, 0x46, 0x46]);

    let alias_export = server.get("/admin/export").await;
    alias_export.assert_status_ok();
    alias_export.assert_header("content-type", "application/zip");
}

#[tokio::test]
async fn admin_export_preserves_gzip_snapshot_warc_payload_and_content_type() {
    let (server, connection) = new_server(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Gzip snapshot export', 'https://example.com/warc', 'https://example.com/warc', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00');
            "#,
        )
        .await;

    let raw_warc = b"WARC/1.0\r\nWARC-Type: response\r\n\r\n<html>gzip export</html>";
    let compressed_warc = hyperlink_artifact_model::compress_snapshot_warc_payload(raw_warc)
        .expect("snapshot warc payload should compress");
    let inserted = hyperlink_artifact_model::insert(
        &connection,
        1,
        None,
        HyperlinkArtifactKind::SnapshotWarc,
        compressed_warc.clone(),
        hyperlink_artifact_model::SNAPSHOT_WARC_GZIP_CONTENT_TYPE,
    )
    .await
    .expect("snapshot_warc artifact should insert");

    let start = server.post("/admin/export/start").await;
    start.assert_status(StatusCode::ACCEPTED);
    let _ = wait_for_backup_ready(&server).await;

    let export = server.get("/admin/export/download").await;
    export.assert_status_ok();

    let mut archive = ZipArchive::new(Cursor::new(export.as_bytes().to_vec()))
        .expect("export should be a valid zip archive");
    let artifacts: Vec<HyperlinkArtifactBackupRow> =
        read_zip_json_file(&mut archive, BACKUP_ARTIFACTS_PATH).expect("artifacts should parse");
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].id, inserted.id);
    assert_eq!(
        artifacts[0].content_type,
        hyperlink_artifact_model::SNAPSHOT_WARC_GZIP_CONTENT_TYPE
    );
    assert_eq!(artifacts[0].size_bytes, compressed_warc.len() as i32);

    let payload = read_zip_binary_file(&mut archive, &artifacts[0].payload_path)
        .expect("artifact payload should exist");
    assert_eq!(payload, compressed_warc);
}

#[tokio::test]
async fn admin_status_reports_queue_and_backup_state() {
    let (server, _) = new_server("").await;

    let status = server.get("/admin/status").await;
    status.assert_status_ok();
    let payload: AdminStatusResponse = status.json();

    assert_eq!(payload.queue.pending, 0);
    assert_eq!(payload.queue.queued, 0);
    assert_eq!(payload.queue.processing, 0);
    assert_eq!(payload.backup.state, "idle");
    assert!(!payload.backup.download_ready);
    assert_eq!(payload.import.state, "idle");
    assert!(!payload.server_time.is_empty());
}

#[tokio::test]
async fn admin_backup_cancel_stops_running_export() {
    let (server, _) = new_server(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Cancel me', 'https://example.com/one', 'https://example.com/one', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00');
            "#,
        )
        .await;

    let start = server.post("/admin/export/start").await;
    assert!(start.status_code() == StatusCode::ACCEPTED || start.status_code() == StatusCode::OK);

    let cancel = server.post("/admin/export/cancel").await;
    cancel.assert_status_see_other();
    cancel.assert_header("location", "/admin/import-export");

    let status = server.get("/admin/status").await;
    status.assert_status_ok();
    let payload: AdminStatusResponse = status.json();
    assert_ne!(payload.backup.state, "running");
}

#[tokio::test]
async fn admin_import_rejects_multipart_without_archive_field() {
    let (server, _) = new_server("").await;

    let multipart = MultipartForm::new().add_part("not_archive", Part::text("ignored"));
    let import = server.post("/admin/import").multipart(multipart).await;
    import.assert_status_see_other();
    import.assert_header("location", "/admin/import-export");

    let status = server.get("/admin/status").await;
    status.assert_status_ok();
    let payload: AdminStatusResponse = status.json();
    assert_eq!(payload.import.state, "idle");
}

#[tokio::test]
async fn admin_import_restores_zip_payload_with_artifacts() {
    let (server, connection) = new_server("").await;

    let hyperlinks = vec![
        HyperlinkBackupRow {
            id: 11,
            title: "Imported One".to_string(),
            url: "https://example.com/one".to_string(),
            raw_url: "https://example.com/one?ref=raw".to_string(),
            og_title: Some("Imported OG".to_string()),
            og_description: None,
            og_type: None,
            og_url: None,
            og_image_url: None,
            og_site_name: None,
            discovery_depth: 0,
            clicks_count: 2,
            last_clicked_at: Some("2026-02-22T01:02:03Z".to_string()),
            created_at: "2026-02-22T00:00:00Z".to_string(),
            updated_at: "2026-02-22T00:00:00Z".to_string(),
        },
        HyperlinkBackupRow {
            id: 12,
            title: "Imported Two".to_string(),
            url: "https://example.com/two".to_string(),
            raw_url: "https://example.com/two".to_string(),
            og_title: None,
            og_description: None,
            og_type: None,
            og_url: None,
            og_image_url: None,
            og_site_name: None,
            discovery_depth: 1,
            clicks_count: 0,
            last_clicked_at: None,
            created_at: "2026-02-22T00:10:00Z".to_string(),
            updated_at: "2026-02-22T00:10:00Z".to_string(),
        },
    ];
    let relations = vec![HyperlinkRelationBackupRow {
        id: 20,
        parent_hyperlink_id: 11,
        child_hyperlink_id: 12,
        created_at: "2026-02-22T00:20:00Z".to_string(),
    }];
    let artifacts = vec![HyperlinkArtifactBackupRow {
        id: 33,
        hyperlink_id: 11,
        kind: HyperlinkArtifactKind::ScreenshotWebp,
        content_type: "image/webp".to_string(),
        size_bytes: 12,
        created_at: "2026-02-22T00:30:00Z".to_string(),
        job_id: None,
        checksum_sha256: None,
        payload_path: "artifacts/33.bin".to_string(),
    }];
    let manifest = BackupManifest {
        version: BACKUP_VERSION,
        exported_at: "2026-02-22T00:40:00Z".to_string(),
        hyperlinks: hyperlinks.len(),
        relations: relations.len(),
        artifacts: artifacts.len(),
    };

    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    write_zip_json_file(&mut writer, BACKUP_MANIFEST_PATH, &manifest)
        .expect("manifest should write");
    write_zip_json_file(&mut writer, BACKUP_HYPERLINKS_PATH, &hyperlinks)
        .expect("hyperlinks should write");
    write_zip_json_file(&mut writer, BACKUP_RELATIONS_PATH, &relations)
        .expect("relations should write");
    write_zip_binary_file_with_compression(
        &mut writer,
        "artifacts/33.bin",
        &[
            0x52, 0x49, 0x46, 0x46, 0x00, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50,
        ],
        CompressionMethod::Deflated,
    )
    .expect("artifact payload should write");
    write_zip_json_file(&mut writer, BACKUP_ARTIFACTS_PATH, &artifacts)
        .expect("artifacts should write");
    let archive_payload = writer
        .finish()
        .expect("zip writer should finish")
        .into_inner();

    let multipart = MultipartForm::new().add_part(
        "archive",
        Part::bytes(archive_payload)
            .file_name("backup.zip")
            .mime_type("application/zip"),
    );
    let import = server.post("/admin/import").multipart(multipart).await;
    import.assert_status_see_other();
    import.assert_header("location", "/admin/import-export");
    let import_status = wait_for_import_ready(&server).await;
    assert_eq!(import_status.import.state, "ready");

    let count = hyperlink::Entity::find()
        .count(&connection)
        .await
        .expect("hyperlink count should succeed");
    assert_eq!(count, 2);

    let imported = hyperlink::Entity::find()
        .order_by_asc(hyperlink::Column::Id)
        .all(&connection)
        .await
        .expect("imported links should load");
    assert_eq!(imported[0].title, "Imported One");
    assert_eq!(imported[0].url, "https://example.com/one");
    assert_eq!(imported[0].raw_url, "https://example.com/one?ref=raw");
    assert_eq!(imported[0].og_title.as_deref(), Some("Imported OG"));
    assert_eq!(imported[1].title, "Imported Two");
    assert_eq!(imported[1].url, "https://example.com/two");

    let relation_count = hyperlink_relation::Entity::find()
        .count(&connection)
        .await
        .expect("relation count should succeed");
    assert_eq!(relation_count, 1);

    let artifact = hyperlink_artifact::Entity::find_by_id(33)
        .one(&connection)
        .await
        .expect("artifact lookup should succeed")
        .expect("artifact should exist");
    assert!(artifact.storage_path.is_some());
    assert!(artifact.payload.is_empty());
    let payload = hyperlink_artifact_model::load_payload(&artifact)
        .await
        .expect("artifact payload should load");
    assert_eq!(
        payload,
        vec![
            0x52, 0x49, 0x46, 0x46, 0x00, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50
        ]
    );
}

#[tokio::test]
async fn admin_import_restores_gzip_snapshot_warc_artifact_and_processing_payload() {
    let (server, connection) = new_server("").await;

    let hyperlinks = vec![HyperlinkBackupRow {
        id: 101,
        title: "Imported gzip WARC".to_string(),
        url: "https://example.com/warc".to_string(),
        raw_url: "https://example.com/warc".to_string(),
        og_title: None,
        og_description: None,
        og_type: None,
        og_url: None,
        og_image_url: None,
        og_site_name: None,
        discovery_depth: 0,
        clicks_count: 0,
        last_clicked_at: None,
        created_at: "2026-02-22T00:00:00Z".to_string(),
        updated_at: "2026-02-22T00:00:00Z".to_string(),
    }];
    let relations: Vec<HyperlinkRelationBackupRow> = Vec::new();
    let raw_warc = b"WARC/1.0\r\nWARC-Type: response\r\n\r\n<html>gzip import</html>";
    let compressed_warc = hyperlink_artifact_model::compress_snapshot_warc_payload(raw_warc)
        .expect("snapshot warc payload should compress");
    let artifacts = vec![HyperlinkArtifactBackupRow {
        id: 202,
        hyperlink_id: 101,
        kind: HyperlinkArtifactKind::SnapshotWarc,
        content_type: hyperlink_artifact_model::SNAPSHOT_WARC_GZIP_CONTENT_TYPE.to_string(),
        size_bytes: compressed_warc.len() as i32,
        created_at: "2026-02-22T00:10:00Z".to_string(),
        job_id: None,
        checksum_sha256: None,
        payload_path: "artifacts/202.bin".to_string(),
    }];
    let manifest = BackupManifest {
        version: BACKUP_VERSION,
        exported_at: "2026-02-22T00:40:00Z".to_string(),
        hyperlinks: hyperlinks.len(),
        relations: relations.len(),
        artifacts: artifacts.len(),
    };

    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    write_zip_json_file(&mut writer, BACKUP_MANIFEST_PATH, &manifest)
        .expect("manifest should write");
    write_zip_json_file(&mut writer, BACKUP_HYPERLINKS_PATH, &hyperlinks)
        .expect("hyperlinks should write");
    write_zip_json_file(&mut writer, BACKUP_RELATIONS_PATH, &relations)
        .expect("relations should write");
    write_zip_binary_file_with_compression(
        &mut writer,
        "artifacts/202.bin",
        &compressed_warc,
        CompressionMethod::Deflated,
    )
    .expect("artifact payload should write");
    write_zip_json_file(&mut writer, BACKUP_ARTIFACTS_PATH, &artifacts)
        .expect("artifacts should write");
    let archive_payload = writer
        .finish()
        .expect("zip writer should finish")
        .into_inner();

    let multipart = MultipartForm::new().add_part(
        "archive",
        Part::bytes(archive_payload)
            .file_name("backup.zip")
            .mime_type("application/zip"),
    );
    let import = server.post("/admin/import").multipart(multipart).await;
    import.assert_status_see_other();
    import.assert_header("location", "/admin/import-export");
    let import_status = wait_for_import_ready(&server).await;
    assert_eq!(import_status.import.state, "ready");

    let artifact = hyperlink_artifact::Entity::find_by_id(202)
        .one(&connection)
        .await
        .expect("artifact lookup should succeed")
        .expect("artifact should exist");
    assert_eq!(
        artifact.content_type,
        hyperlink_artifact_model::SNAPSHOT_WARC_GZIP_CONTENT_TYPE
    );
    assert_eq!(artifact.size_bytes, compressed_warc.len() as i32);
    assert!(artifact.storage_path.is_some());
    assert!(artifact.payload.is_empty());

    let stored_payload = hyperlink_artifact_model::load_payload(&artifact)
        .await
        .expect("stored payload should load");
    assert_eq!(stored_payload, compressed_warc);

    let processing_payload = hyperlink_artifact_model::load_processing_payload(&artifact)
        .await
        .expect("processing payload should decode");
    assert_eq!(processing_payload, raw_warc);
}

#[tokio::test]
async fn admin_import_cancel_stops_running_import_job() {
    let (server, _) = new_server("").await;

    let hyperlinks = vec![HyperlinkBackupRow {
        id: 501,
        title: "Import cancel target".to_string(),
        url: "https://example.com/cancel".to_string(),
        raw_url: "https://example.com/cancel".to_string(),
        og_title: None,
        og_description: None,
        og_type: None,
        og_url: None,
        og_image_url: None,
        og_site_name: None,
        discovery_depth: 0,
        clicks_count: 0,
        last_clicked_at: None,
        created_at: "2026-02-22T00:00:00Z".to_string(),
        updated_at: "2026-02-22T00:00:00Z".to_string(),
    }];
    let relations: Vec<HyperlinkRelationBackupRow> = Vec::new();

    let artifact_count = 64usize;
    let payload_size = 256 * 1024usize;
    let payload = vec![0x5Au8; payload_size];
    let artifacts: Vec<HyperlinkArtifactBackupRow> = (0..artifact_count)
        .map(|offset| {
            let id = 7000 + offset as i32;
            HyperlinkArtifactBackupRow {
                id,
                hyperlink_id: 501,
                kind: HyperlinkArtifactKind::ScreenshotWebp,
                content_type: "image/webp".to_string(),
                size_bytes: payload_size as i32,
                created_at: "2026-02-22T00:10:00Z".to_string(),
                job_id: None,
                checksum_sha256: None,
                payload_path: format!("artifacts/{id}.bin"),
            }
        })
        .collect();
    let manifest = BackupManifest {
        version: BACKUP_VERSION,
        exported_at: "2026-02-22T00:40:00Z".to_string(),
        hyperlinks: hyperlinks.len(),
        relations: relations.len(),
        artifacts: artifacts.len(),
    };

    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    write_zip_json_file(&mut writer, BACKUP_MANIFEST_PATH, &manifest)
        .expect("manifest should write");
    write_zip_json_file(&mut writer, BACKUP_HYPERLINKS_PATH, &hyperlinks)
        .expect("hyperlinks should write");
    write_zip_json_file(&mut writer, BACKUP_RELATIONS_PATH, &relations)
        .expect("relations should write");
    for artifact in &artifacts {
        write_zip_binary_file_with_compression(
            &mut writer,
            &artifact.payload_path,
            &payload,
            CompressionMethod::Stored,
        )
        .expect("artifact payload should write");
    }
    write_zip_json_file(&mut writer, BACKUP_ARTIFACTS_PATH, &artifacts)
        .expect("artifacts should write");
    let archive_payload = writer
        .finish()
        .expect("zip writer should finish")
        .into_inner();

    let multipart = MultipartForm::new().add_part(
        "archive",
        Part::bytes(archive_payload)
            .file_name("backup.zip")
            .mime_type("application/zip"),
    );
    let import = server.post("/admin/import").multipart(multipart).await;
    import.assert_status_see_other();
    import.assert_header("location", "/admin/import-export");

    let running = wait_for_import_running(&server).await;
    assert_eq!(running.import.state, "running");

    let cancel = server.post("/admin/import/cancel").await;
    cancel.assert_status_see_other();
    cancel.assert_header("location", "/admin/import-export");

    let terminal = wait_for_import_terminal(&server).await;
    assert_eq!(terminal.import.state, "cancelled");
}
