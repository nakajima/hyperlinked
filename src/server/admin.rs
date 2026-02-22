use std::collections::{HashMap, HashSet};

use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Response,
    routing,
};
use sailfish::Template;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QuerySelect};

use crate::{
    entity::{
        hyperlink,
        hyperlink_artifact::{self, HyperlinkArtifactKind},
        hyperlink_processing_job::{self, HyperlinkProcessingJobKind, HyperlinkProcessingJobState},
    },
    model::hyperlink_processing_job::{
        self as hyperlink_processing_job_model, ProcessingQueueSender,
    },
    server::{
        context::Context,
        flash::{Flash, FlashName, redirect_with_flash},
    },
};

use super::views;

pub fn routes() -> Router<Context> {
    Router::new().route("/admin", routing::get(index)).route(
        "/admin/process-missing-artifacts",
        routing::post(process_missing_artifacts),
    )
}

#[derive(Clone, Copy, Debug)]
struct LastRunSummary {
    snapshot_queued: usize,
    oembed_queued: usize,
    readability_queued: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct MissingArtifactsSummary {
    total_hyperlinks: usize,
    missing_source: usize,
    missing_oembed: usize,
    missing_readability: usize,
    snapshot_already_processing: usize,
    oembed_already_processing: usize,
    readability_already_processing: usize,
    snapshot_will_queue: usize,
    oembed_will_queue: usize,
    readability_will_queue: usize,
}

#[derive(Default)]
struct ArtifactPresence {
    has_source: bool,
    has_oembed_meta: bool,
    has_readable_text: bool,
    has_readable_meta: bool,
}

struct MissingArtifactsPlan {
    summary: MissingArtifactsSummary,
    snapshot_hyperlink_ids: Vec<i32>,
    oembed_hyperlink_ids: Vec<i32>,
    readability_hyperlink_ids: Vec<i32>,
}

async fn index(State(state): State<Context>, headers: HeaderMap) -> Response {
    let plan = match build_missing_artifacts_plan(&state.connection).await {
        Ok(plan) => plan,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load missing-artifact summary: {err}"),
            );
        }
    };

    views::render_html_page_with_flash(
        "Admin",
        render_index(&plan.summary),
        Flash::from_headers(&headers),
    )
}

async fn process_missing_artifacts(State(state): State<Context>, headers: HeaderMap) -> Response {
    let plan = match build_missing_artifacts_plan(&state.connection).await {
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
        "/admin",
        FlashName::Notice,
        format!(
            "Queued {} snapshot job(s), {} oembed job(s), and {} readability job(s).",
            result.snapshot_queued, result.oembed_queued, result.readability_queued
        ),
    )
}

async fn build_missing_artifacts_plan(
    connection: &DatabaseConnection,
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
            oembed_hyperlink_ids: Vec::new(),
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
            HyperlinkArtifactKind::OembedMeta,
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
            HyperlinkArtifactKind::OembedMeta => {
                presence.has_oembed_meta = true;
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
            HyperlinkProcessingJobKind::Oembed,
            HyperlinkProcessingJobKind::Readability,
        ]))
        .into_tuple::<(i32, HyperlinkProcessingJobKind)>()
        .all(connection)
        .await?;

    let mut snapshot_active_hyperlinks = HashSet::<i32>::new();
    let mut oembed_active_hyperlinks = HashSet::<i32>::new();
    let mut readability_active_hyperlinks = HashSet::<i32>::new();
    for (hyperlink_id, kind) in active_rows {
        match kind {
            HyperlinkProcessingJobKind::Snapshot => {
                snapshot_active_hyperlinks.insert(hyperlink_id);
            }
            HyperlinkProcessingJobKind::Oembed => {
                oembed_active_hyperlinks.insert(hyperlink_id);
            }
            HyperlinkProcessingJobKind::Readability => {
                readability_active_hyperlinks.insert(hyperlink_id);
            }
            _ => {}
        }
    }

    let mut snapshot_hyperlink_ids = Vec::new();
    let mut oembed_hyperlink_ids = Vec::new();
    let mut readability_hyperlink_ids = Vec::new();

    for hyperlink_id in hyperlink_ids {
        let presence = artifact_presence_by_hyperlink.get(&hyperlink_id);
        let has_source = presence.is_some_and(|presence| presence.has_source);
        if !has_source {
            summary.missing_source += 1;
            if snapshot_active_hyperlinks.contains(&hyperlink_id) {
                summary.snapshot_already_processing += 1;
            } else {
                snapshot_hyperlink_ids.push(hyperlink_id);
            }
            continue;
        }

        let has_oembed_meta = presence.is_some_and(|presence| presence.has_oembed_meta);
        if !has_oembed_meta {
            summary.missing_oembed += 1;
            if oembed_active_hyperlinks.contains(&hyperlink_id) {
                summary.oembed_already_processing += 1;
            } else {
                oembed_hyperlink_ids.push(hyperlink_id);
            }
        }

        let has_readable_artifacts = presence
            .is_some_and(|presence| presence.has_readable_text && presence.has_readable_meta);
        if !has_readable_artifacts {
            summary.missing_readability += 1;
            if readability_active_hyperlinks.contains(&hyperlink_id) {
                summary.readability_already_processing += 1;
            } else {
                readability_hyperlink_ids.push(hyperlink_id);
            }
        }
    }

    summary.snapshot_will_queue = snapshot_hyperlink_ids.len();
    summary.oembed_will_queue = oembed_hyperlink_ids.len();
    summary.readability_will_queue = readability_hyperlink_ids.len();

    Ok(MissingArtifactsPlan {
        summary,
        snapshot_hyperlink_ids,
        oembed_hyperlink_ids,
        readability_hyperlink_ids,
    })
}

async fn execute_missing_artifacts_plan(
    connection: &DatabaseConnection,
    queue: Option<&ProcessingQueueSender>,
    plan: MissingArtifactsPlan,
) -> Result<LastRunSummary, sea_orm::DbErr> {
    for hyperlink_id in &plan.snapshot_hyperlink_ids {
        hyperlink_processing_job_model::enqueue_for_hyperlink_kind(
            connection,
            *hyperlink_id,
            HyperlinkProcessingJobKind::Snapshot,
            queue,
        )
        .await?;
    }
    for hyperlink_id in &plan.oembed_hyperlink_ids {
        hyperlink_processing_job_model::enqueue_for_hyperlink_kind(
            connection,
            *hyperlink_id,
            HyperlinkProcessingJobKind::Oembed,
            queue,
        )
        .await?;
    }
    for hyperlink_id in &plan.readability_hyperlink_ids {
        hyperlink_processing_job_model::enqueue_for_hyperlink_kind(
            connection,
            *hyperlink_id,
            HyperlinkProcessingJobKind::Readability,
            queue,
        )
        .await?;
    }

    Ok(LastRunSummary {
        snapshot_queued: plan.snapshot_hyperlink_ids.len(),
        oembed_queued: plan.oembed_hyperlink_ids.len(),
        readability_queued: plan.readability_hyperlink_ids.len(),
    })
}

#[derive(Template)]
#[template(path = "admin/index.stpl")]
struct AdminIndexTemplate<'a> {
    summary: &'a MissingArtifactsSummary,
    has_missing_artifacts_to_process: bool,
}

fn render_index(summary: &MissingArtifactsSummary) -> Result<String, sailfish::RenderError> {
    AdminIndexTemplate {
        summary,
        has_missing_artifacts_to_process: summary.snapshot_will_queue > 0
            || summary.oembed_will_queue > 0
            || summary.readability_will_queue > 0,
    }
    .render()
}

fn response_error(status: StatusCode, message: impl Into<String>) -> Response {
    views::render_error_page(status, message, "/admin", "Back to admin")
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};

    use super::*;
    use crate::server::test_support;

    async fn new_server(seed_sql: &str) -> (axum_test::TestServer, sea_orm::DatabaseConnection) {
        let connection = test_support::new_memory_connection().await;
        test_support::initialize_hyperlinks_schema(&connection).await;
        test_support::execute_sql(&connection, seed_sql).await;

        let app = Router::<Context>::new()
            .merge(routes())
            .with_state(Context {
                connection: connection.clone(),
                processing_queue: None,
            });
        (
            axum_test::TestServer::new(app).expect("test server should initialize"),
            connection,
        )
    }

    #[tokio::test]
    async fn process_missing_artifacts_enqueues_snapshot_oembed_and_readability() {
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
                    (3, 3, NULL, 'readable_text', X'74657874', 'text/markdown; charset=utf-8', 4, '2026-02-21 00:01:00'),
                    (4, 3, NULL, 'readable_meta', X'7B7D', 'application/json', 2, '2026-02-21 00:01:00');
            "#,
        )
        .await;

        let action = server.post("/admin/process-missing-artifacts").await;
        action.assert_status_see_other();
        action.assert_header("location", "/admin");

        let snapshot_jobs = hyperlink_processing_job::Entity::find()
            .filter(hyperlink_processing_job::Column::Kind.eq(HyperlinkProcessingJobKind::Snapshot))
            .count(&connection)
            .await
            .expect("snapshot jobs count should succeed");
        assert_eq!(snapshot_jobs, 1);

        let readability_jobs = hyperlink_processing_job::Entity::find()
            .filter(
                hyperlink_processing_job::Column::Kind.eq(HyperlinkProcessingJobKind::Readability),
            )
            .count(&connection)
            .await
            .expect("readability jobs count should succeed");
        assert_eq!(readability_jobs, 1);

        let oembed_jobs = hyperlink_processing_job::Entity::find()
            .filter(hyperlink_processing_job::Column::Kind.eq(HyperlinkProcessingJobKind::Oembed))
            .count(&connection)
            .await
            .expect("oembed jobs count should succeed");
        assert_eq!(oembed_jobs, 2);
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

        let page = server.get("/admin").await;
        page.assert_status_ok();
        let body = page.text();
        assert!(body.contains("Process all missing artifacts"));
        assert!(body.contains("Missing source"));
        assert!(body.contains("Missing oEmbed"));
        assert!(body.contains("Missing readability"));
        assert!(body.contains("Snapshot to queue"));
        assert!(body.contains("oEmbed to queue"));
        assert!(body.contains("Readability to queue"));
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
        action.assert_header("location", "/admin");
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
                    (2, 1, NULL, 'oembed_meta', X'7B7D', 'application/json', 2, '2026-02-21 00:01:00'),
                    (3, 1, NULL, 'readable_text', X'74657874', 'text/markdown; charset=utf-8', 4, '2026-02-21 00:01:00'),
                    (4, 1, NULL, 'readable_meta', X'7B7D', 'application/json', 2, '2026-02-21 00:01:00');
            "#,
        )
        .await;

        let page = server.get("/admin").await;
        page.assert_status_ok();
        let body = page.text();
        assert!(body.contains(
            "<input type=\"submit\" value=\"Process all missing artifacts\" disabled />"
        ));
    }

    #[tokio::test]
    async fn admin_page_shows_flash_style_examples() {
        let (server, _) = new_server("").await;

        let page = server.get("/admin").await;
        page.assert_status_ok();
        let body = page.text();
        assert!(body.contains("Flash examples"));
        assert!(body.contains("border-notice-border"));
        assert!(body.contains("border-invalid"));
        assert!(body.contains("border-dev-alert-border"));
    }
}
