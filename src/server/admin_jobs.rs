use std::collections::{HashMap, HashSet};

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing,
};
use sailfish::Template;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, DbErr, EntityTrait, QueryFilter,
    QueryResult, Statement,
};
use serde::{Deserialize, Serialize};

use crate::{
    entity::{hyperlink, hyperlink_processing_job},
    model::hyperlink_processing_job as hyperlink_processing_job_model,
    queue::ProcessingTask,
    server::{context::Context, flash::Flash, views},
};

const DEFAULT_LIMIT: u64 = 50;
const MAX_LIMIT: u64 = 200;

pub fn routes() -> Router<Context> {
    Router::new()
        .route("/admin/jobs", routing::get(index))
        .route("/admin/jobs/pending-count", routing::get(pending_count))
}

#[derive(Clone, Debug, Deserialize)]
struct AdminJobsQuery {
    status: Option<String>,
    limit: Option<u64>,
    page: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueueStatusFilter {
    All,
    Queued,
    Processing,
    Completed,
    Failed,
}

impl QueueStatusFilter {
    fn from_query(value: Option<&str>) -> Self {
        match value.unwrap_or("all") {
            "queued" => Self::Queued,
            "processing" => Self::Processing,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => Self::All,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Queued => "queued",
            Self::Processing => "processing",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, Default)]
struct QueueStats {
    total: i64,
    queued: i64,
    processing: i64,
    completed: i64,
    failed: i64,
}

#[derive(Clone, Debug)]
struct QueueJobRow {
    queue_id: i64,
    queue_status: String,
    payload: String,
    attempts: i32,
    max_attempts: i32,
    last_error: Option<String>,
    queued_ms_total: i64,
    queued_ms_last: Option<i64>,
    processing_ms_total: i64,
    processing_ms_last: Option<i64>,
    processing_job_id: Option<i32>,
}

#[derive(Clone, Debug)]
struct AdminJobRowView {
    queue_id: i64,
    queue_status: String,
    processing_job_id: Option<i32>,
    processing_state: Option<String>,
    processing_kind: Option<String>,
    hyperlink_id: Option<i32>,
    hyperlink_title: Option<String>,
    hyperlink_url: Option<String>,
    attempts_display: String,
    queued_timing_display: String,
    processing_timing_display: String,
    error_display: Option<String>,
    payload_excerpt: String,
    payload_unmapped: bool,
}

#[derive(Template)]
#[template(path = "admin/jobs.stpl")]
struct AdminJobsTemplate<'a> {
    stats: &'a QueueStats,
    rows: &'a [AdminJobRowView],
    status_filter: &'a str,
    limit: u64,
    page: u64,
    total_pages: u64,
    total_rows: i64,
    range_start: i64,
    range_end: i64,
    prev_page_href: Option<String>,
    next_page_href: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PendingCountResponse {
    pending: i64,
    queued: i64,
    processing: i64,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

async fn index(
    State(state): State<Context>,
    headers: HeaderMap,
    Query(query): Query<AdminJobsQuery>,
) -> Response {
    let status_filter = QueueStatusFilter::from_query(query.status.as_deref());
    let limit = resolve_limit(query.limit);
    let requested_page = resolve_page(query.page);

    let stats = match fetch_queue_stats(&state.connection).await {
        Ok(stats) => stats,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load queue stats: {err}"),
            );
        }
    };

    let total_rows = match fetch_filtered_total(&state.connection, status_filter).await {
        Ok(total) => total,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load filtered queue count: {err}"),
            );
        }
    };

    let total_pages = total_pages(total_rows, limit);
    let page = requested_page.min(total_pages.max(1));
    let offset = page_offset(page, limit);

    let queue_rows = match fetch_queue_rows(&state.connection, status_filter, limit, offset).await {
        Ok(rows) => rows,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load queue rows: {err}"),
            );
        }
    };

    let rows = match enrich_queue_rows(&state.connection, queue_rows).await {
        Ok(rows) => rows,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load queue row context: {err}"),
            );
        }
    };

    views::render_html_page_with_flash(
        "Jobs",
        render_index(
            &stats,
            &rows,
            status_filter,
            limit,
            page,
            total_pages,
            total_rows,
        ),
        Flash::from_headers(&headers),
    )
}

async fn pending_count(State(state): State<Context>) -> Response {
    match fetch_queue_stats(&state.connection).await {
        Ok(stats) => Json(PendingCountResponse {
            pending: stats.queued + stats.processing,
            queued: stats.queued,
            processing: stats.processing,
        })
        .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("failed to load pending queue count: {err}"),
            }),
        )
            .into_response(),
    }
}

fn render_index(
    stats: &QueueStats,
    rows: &[AdminJobRowView],
    status_filter: QueueStatusFilter,
    limit: u64,
    page: u64,
    total_pages: u64,
    total_rows: i64,
) -> Result<String, sailfish::RenderError> {
    let range_start = if total_rows == 0 {
        0
    } else {
        i64::try_from(page_offset(page, limit)).unwrap_or(i64::MAX) + 1
    };
    let range_end = if rows.is_empty() {
        0
    } else {
        range_start + i64::try_from(rows.len()).unwrap_or(0) - 1
    };

    let prev_page_href = if page > 1 {
        Some(jobs_href(status_filter, limit, page - 1))
    } else {
        None
    };
    let next_page_href = if page < total_pages {
        Some(jobs_href(status_filter, limit, page + 1))
    } else {
        None
    };

    AdminJobsTemplate {
        stats,
        rows,
        status_filter: status_filter.as_str(),
        limit,
        page,
        total_pages,
        total_rows,
        range_start,
        range_end,
        prev_page_href,
        next_page_href,
    }
    .render()
}

async fn fetch_queue_stats(connection: &DatabaseConnection) -> Result<QueueStats, DbErr> {
    let statement = Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "SELECT
             COUNT(*) AS total,
             COALESCE(SUM(CASE WHEN j.status = 'queued' THEN 1 ELSE 0 END), 0) AS queued,
             COALESCE(SUM(CASE WHEN j.status = 'processing' THEN 1 ELSE 0 END), 0) AS processing,
             COALESCE(SUM(CASE WHEN j.status = 'completed' THEN 1 ELSE 0 END), 0) AS completed,
             COALESCE(SUM(CASE WHEN j.status = 'failed' OR hpj.state = 'failed' THEN 1 ELSE 0 END), 0) AS failed
         FROM jobs j
         LEFT JOIN hyperlink_processing_job hpj
           ON hpj.id = CASE
               WHEN json_valid(j.payload) THEN CAST(json_extract(j.payload, '$.processing_job_id') AS INTEGER)
               ELSE NULL
             END
         WHERE j.job_type = ?"
            .to_string(),
        vec![processing_task_job_type().into()],
    );

    let Some(row) = connection.query_one(statement).await? else {
        return Ok(QueueStats::default());
    };
    Ok(QueueStats {
        total: try_get_by_index::<i64>(&row, 0)?,
        queued: try_get_by_index::<i64>(&row, 1)?,
        processing: try_get_by_index::<i64>(&row, 2)?,
        completed: try_get_by_index::<i64>(&row, 3)?,
        failed: try_get_by_index::<i64>(&row, 4)?,
    })
}

async fn fetch_filtered_total(
    connection: &DatabaseConnection,
    status_filter: QueueStatusFilter,
) -> Result<i64, DbErr> {
    let mut sql = String::from(
        "SELECT COUNT(*) AS total
         FROM jobs j
         LEFT JOIN hyperlink_processing_job hpj
           ON hpj.id = CASE
               WHEN json_valid(j.payload) THEN CAST(json_extract(j.payload, '$.processing_job_id') AS INTEGER)
               ELSE NULL
             END
         WHERE j.job_type = ?",
    );
    let mut values = vec![processing_task_job_type().into()];
    append_status_filter_clause(&mut sql, &mut values, status_filter);

    let statement = Statement::from_sql_and_values(DbBackend::Sqlite, sql, values);
    let Some(row) = connection.query_one(statement).await? else {
        return Ok(0);
    };
    try_get_by_index::<i64>(&row, 0)
}

async fn fetch_queue_rows(
    connection: &DatabaseConnection,
    status_filter: QueueStatusFilter,
    limit: u64,
    offset: u64,
) -> Result<Vec<QueueJobRow>, DbErr> {
    let mut sql = String::from(
        "SELECT
             j.id,
             j.status,
             j.payload,
             j.attempts,
             j.max_attempts,
             j.last_error,
             COALESCE(j.queued_ms_total, 0) AS queued_ms_total,
             j.queued_ms_last,
             COALESCE(j.processing_ms_total, 0) AS processing_ms_total,
             j.processing_ms_last
         FROM jobs j
         LEFT JOIN hyperlink_processing_job hpj
           ON hpj.id = CASE
               WHEN json_valid(j.payload) THEN CAST(json_extract(j.payload, '$.processing_job_id') AS INTEGER)
               ELSE NULL
             END
         WHERE j.job_type = ?",
    );

    let mut values = vec![processing_task_job_type().into()];
    append_status_filter_clause(&mut sql, &mut values, status_filter);
    sql.push_str(" ORDER BY j.id DESC LIMIT ? OFFSET ?");
    values.push(i64::try_from(limit).unwrap_or(i64::MAX).into());
    values.push(i64::try_from(offset).unwrap_or(i64::MAX).into());

    let statement = Statement::from_sql_and_values(DbBackend::Sqlite, sql, values);
    let rows = connection.query_all(statement).await?;
    let mut parsed_rows = Vec::with_capacity(rows.len());

    for row in rows {
        let payload = try_get_by_index::<String>(&row, 2)?;
        let processing_job_id = serde_json::from_str::<ProcessingTask>(&payload)
            .ok()
            .map(|task| task.processing_job_id);

        parsed_rows.push(QueueJobRow {
            queue_id: try_get_by_index::<i64>(&row, 0)?,
            queue_status: try_get_by_index::<String>(&row, 1)?,
            payload,
            attempts: try_get_by_index::<i32>(&row, 3)?,
            max_attempts: try_get_by_index::<i32>(&row, 4)?,
            last_error: try_get_by_index::<Option<String>>(&row, 5)?,
            queued_ms_total: try_get_by_index::<i64>(&row, 6)?,
            queued_ms_last: try_get_by_index::<Option<i64>>(&row, 7)?,
            processing_ms_total: try_get_by_index::<i64>(&row, 8)?,
            processing_ms_last: try_get_by_index::<Option<i64>>(&row, 9)?,
            processing_job_id,
        });
    }

    Ok(parsed_rows)
}

async fn enrich_queue_rows(
    connection: &DatabaseConnection,
    queue_rows: Vec<QueueJobRow>,
) -> Result<Vec<AdminJobRowView>, DbErr> {
    let processing_job_ids: HashSet<i32> = queue_rows
        .iter()
        .filter_map(|row| row.processing_job_id)
        .collect();

    let processing_jobs: HashMap<i32, hyperlink_processing_job::Model> =
        if processing_job_ids.is_empty() {
            HashMap::new()
        } else {
            hyperlink_processing_job::Entity::find()
                .filter(hyperlink_processing_job::Column::Id.is_in(processing_job_ids))
                .all(connection)
                .await?
                .into_iter()
                .map(|job| (job.id, job))
                .collect()
        };

    let hyperlink_ids: HashSet<i32> = processing_jobs
        .values()
        .map(|job| job.hyperlink_id)
        .collect();
    let hyperlinks: HashMap<i32, hyperlink::Model> = if hyperlink_ids.is_empty() {
        HashMap::new()
    } else {
        hyperlink::Entity::find()
            .filter(hyperlink::Column::Id.is_in(hyperlink_ids))
            .all(connection)
            .await?
            .into_iter()
            .map(|link| (link.id, link))
            .collect()
    };

    let mut rows = Vec::with_capacity(queue_rows.len());
    for row in queue_rows {
        let processing_job = row
            .processing_job_id
            .and_then(|processing_job_id| processing_jobs.get(&processing_job_id));

        let processing_state = processing_job
            .map(|job| hyperlink_processing_job_model::state_name(job.state.clone()).to_string());
        let processing_kind = processing_job
            .map(|job| hyperlink_processing_job_model::kind_name(job.kind.clone()).to_string());
        let processing_error = processing_job.and_then(|job| job.error_message.clone());

        let hyperlink_id = processing_job.map(|job| job.hyperlink_id);
        let hyperlink_model = hyperlink_id.and_then(|id| hyperlinks.get(&id));
        let hyperlink_title = hyperlink_model.map(|link| link.title.clone());
        let hyperlink_url = hyperlink_model.map(|link| link.url.clone());

        let error_display = match (processing_error.as_deref(), row.last_error.as_deref()) {
            (Some(processing), Some(queue)) if processing == queue => Some(processing.to_string()),
            (Some(processing), Some(queue)) => Some(format!("{processing} (queue: {queue})")),
            (Some(processing), None) => Some(processing.to_string()),
            (None, Some(queue)) => Some(queue.to_string()),
            (None, None) => None,
        };

        rows.push(AdminJobRowView {
            queue_id: row.queue_id,
            queue_status: row.queue_status,
            processing_job_id: row.processing_job_id,
            processing_state,
            processing_kind,
            hyperlink_id,
            hyperlink_title,
            hyperlink_url,
            attempts_display: format!("{}/{}", row.attempts, row.max_attempts),
            queued_timing_display: format_timing(row.queued_ms_total, row.queued_ms_last),
            processing_timing_display: format_timing(
                row.processing_ms_total,
                row.processing_ms_last,
            ),
            error_display,
            payload_excerpt: truncate(&row.payload, 120),
            payload_unmapped: row.processing_job_id.is_none(),
        });
    }

    Ok(rows)
}

fn response_error(status: StatusCode, message: impl Into<String>) -> Response {
    views::render_error_page(status, message, "/admin/jobs", "Back to jobs dashboard")
}

fn processing_task_job_type() -> &'static str {
    std::any::type_name::<ProcessingTask>()
}

fn resolve_limit(limit: Option<u64>) -> u64 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

fn resolve_page(page: Option<u64>) -> u64 {
    page.unwrap_or(1).max(1)
}

fn total_pages(total_rows: i64, limit: u64) -> u64 {
    if total_rows <= 0 {
        return 1;
    }

    let total_rows_u64 = u64::try_from(total_rows).unwrap_or(u64::MAX);
    total_rows_u64.div_ceil(limit.max(1))
}

fn page_offset(page: u64, limit: u64) -> u64 {
    page.saturating_sub(1).saturating_mul(limit.max(1))
}

fn jobs_href(status_filter: QueueStatusFilter, limit: u64, page: u64) -> String {
    format!(
        "/admin/jobs?status={}&limit={}&page={}",
        status_filter.as_str(),
        limit,
        page
    )
}

fn append_status_filter_clause(
    sql: &mut String,
    values: &mut Vec<sea_orm::Value>,
    status_filter: QueueStatusFilter,
) {
    match status_filter {
        QueueStatusFilter::All => {}
        QueueStatusFilter::Queued => {
            sql.push_str(" AND j.status = ?");
            values.push("queued".into());
        }
        QueueStatusFilter::Processing => {
            sql.push_str(" AND j.status = ?");
            values.push("processing".into());
        }
        QueueStatusFilter::Completed => {
            sql.push_str(" AND j.status = ?");
            values.push("completed".into());
        }
        QueueStatusFilter::Failed => {
            sql.push_str(" AND (j.status = ? OR hpj.state = ?)");
            values.push("failed".into());
            values.push("failed".into());
        }
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let mut out = String::new();
    for _ in 0..max_chars {
        let Some(ch) = chars.next() else {
            return out;
        };
        out.push(ch);
    }
    if chars.next().is_some() {
        out.push_str("...");
    }
    out
}

fn format_timing(total_ms: i64, last_ms: Option<i64>) -> String {
    format!(
        "{total_ms} ms total | {} ms last",
        last_ms.unwrap_or_default()
    )
}

fn try_get_by_index<T>(row: &QueryResult, index: usize) -> Result<T, DbErr>
where
    T: sea_orm::TryGetable,
{
    row.try_get_by_index(index)
        .map_err(|err| DbErr::Custom(format!("failed to decode row index {index}: {err:?}").into()))
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};

    use super::*;
    use crate::server::test_support;

    async fn new_server(seed_sql: &str) -> (axum_test::TestServer, DatabaseConnection) {
        let connection = test_support::new_memory_connection().await;
        test_support::initialize_hyperlinks_schema(&connection).await;
        test_support::initialize_queue_jobs_schema(&connection).await;
        if !seed_sql.is_empty() {
            test_support::execute_sql(&connection, seed_sql).await;
        }

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

    async fn insert_queue_job(
        connection: &DatabaseConnection,
        id: i64,
        status: &str,
        payload: &str,
        last_error: Option<&str>,
        job_type: &str,
    ) {
        let statement = Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO jobs (
                id, job_type, payload, status, attempts, max_attempts, available_at, last_error, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
                .to_string(),
            vec![
                id.into(),
                job_type.into(),
                payload.into(),
                status.into(),
                0.into(),
                20.into(),
                1.into(),
                last_error.map(|value| value.to_string()).into(),
                1.into(),
                1.into(),
            ],
        );
        connection
            .execute(statement)
            .await
            .expect("queue row should insert");
    }

    #[tokio::test]
    async fn jobs_dashboard_renders_rows_with_joined_hyperlink_context() {
        let (server, connection) = new_server(
            r#"
                INSERT INTO hyperlink (id, title, url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example title', 'https://example.com', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00');
                INSERT INTO hyperlink_processing_job (id, hyperlink_id, kind, state, error_message, queued_at, started_at, finished_at, created_at, updated_at)
                VALUES (101, 1, 'snapshot', 'running', NULL, '2026-02-21 00:01:00', '2026-02-21 00:01:10', NULL, '2026-02-21 00:01:00', '2026-02-21 00:01:10');
            "#,
        )
        .await;

        insert_queue_job(
            &connection,
            500,
            "processing",
            r#"{"processing_job_id":101}"#,
            None,
            processing_task_job_type(),
        )
        .await;

        let page = server.get("/admin/jobs").await;
        page.assert_status_ok();
        let body = page.text();

        assert!(body.contains("Jobs dashboard"));
        assert!(body.contains("queue#500"));
        assert!(body.contains("Example title"));
        assert!(body.contains("https://example.com"));
        assert!(body.contains("snapshot"));
        assert!(body.contains("running"));
        assert!(body.contains("/hyperlinks/1"));
    }

    #[tokio::test]
    async fn jobs_dashboard_status_filter_applies() {
        let (server, connection) = new_server("").await;

        insert_queue_job(
            &connection,
            1,
            "queued",
            r#"{"processing_job_id":1}"#,
            None,
            processing_task_job_type(),
        )
        .await;
        insert_queue_job(
            &connection,
            2,
            "processing",
            r#"{"processing_job_id":2}"#,
            None,
            processing_task_job_type(),
        )
        .await;
        insert_queue_job(
            &connection,
            3,
            "failed",
            r#"{"processing_job_id":3}"#,
            Some("failed"),
            processing_task_job_type(),
        )
        .await;

        let page = server.get("/admin/jobs?status=processing").await;
        page.assert_status_ok();
        let body = page.text();

        assert!(body.contains("queue#2"));
        assert!(!body.contains("queue#1"));
        assert!(!body.contains("queue#3"));
    }

    #[tokio::test]
    async fn jobs_dashboard_paginates_rows() {
        let (server, connection) = new_server("").await;

        insert_queue_job(
            &connection,
            1,
            "queued",
            r#"{"processing_job_id":1}"#,
            None,
            processing_task_job_type(),
        )
        .await;
        insert_queue_job(
            &connection,
            2,
            "queued",
            r#"{"processing_job_id":2}"#,
            None,
            processing_task_job_type(),
        )
        .await;
        insert_queue_job(
            &connection,
            3,
            "queued",
            r#"{"processing_job_id":3}"#,
            None,
            processing_task_job_type(),
        )
        .await;

        let page = server.get("/admin/jobs?status=all&limit=1&page=2").await;
        page.assert_status_ok();
        let body = page.text();

        assert!(!body.contains("queue#3"));
        assert!(body.contains("queue#2"));
        assert!(!body.contains("queue#1"));
        assert!(body.contains("Page 2 of 3"));
        assert!(body.contains("/admin/jobs?status=all&amp;limit=1&amp;page=1"));
        assert!(body.contains("/admin/jobs?status=all&amp;limit=1&amp;page=3"));
    }

    #[tokio::test]
    async fn jobs_dashboard_clamps_page_to_last_available_page() {
        let (server, connection) = new_server("").await;

        insert_queue_job(
            &connection,
            1,
            "queued",
            r#"{"processing_job_id":1}"#,
            None,
            processing_task_job_type(),
        )
        .await;
        insert_queue_job(
            &connection,
            2,
            "queued",
            r#"{"processing_job_id":2}"#,
            None,
            processing_task_job_type(),
        )
        .await;

        let page = server.get("/admin/jobs?status=all&limit=1&page=99").await;
        page.assert_status_ok();
        let body = page.text();

        assert!(body.contains("queue#1"));
        assert!(!body.contains("queue#2"));
        assert!(body.contains("Page 2 of 2"));
    }

    #[tokio::test]
    async fn jobs_dashboard_failed_filter_includes_processing_failed_rows() {
        let (server, connection) = new_server(
            r#"
                INSERT INTO hyperlink (id, title, url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (9, 'Failed Link', 'https://failed.example', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00');
                INSERT INTO hyperlink_processing_job (id, hyperlink_id, kind, state, error_message, queued_at, started_at, finished_at, created_at, updated_at)
                VALUES (909, 9, 'readability', 'failed', 'readability crash', '2026-02-21 00:01:00', '2026-02-21 00:01:10', '2026-02-21 00:01:20', '2026-02-21 00:01:00', '2026-02-21 00:01:20');
            "#,
        )
        .await;

        // Queue row can be completed while the domain processing state is failed.
        insert_queue_job(
            &connection,
            99,
            "completed",
            r#"{"processing_job_id":909}"#,
            None,
            processing_task_job_type(),
        )
        .await;

        let page = server.get("/admin/jobs?status=failed").await;
        page.assert_status_ok();
        let body = page.text();

        assert!(body.contains("queue#99"));
        assert!(body.contains("Failed Link"));
        assert!(body.contains("readability / failed"));
    }

    #[tokio::test]
    async fn jobs_dashboard_handles_unparseable_payload_without_500() {
        let (server, connection) = new_server("").await;

        insert_queue_job(
            &connection,
            1,
            "queued",
            "this is not json",
            None,
            processing_task_job_type(),
        )
        .await;

        let page = server.get("/admin/jobs").await;
        page.assert_status_ok();
        let body = page.text();

        assert!(body.contains("queue#1"));
        assert!(body.contains("Unmapped payload"));
        assert!(body.contains("this is not json"));
    }

    #[tokio::test]
    async fn pending_count_includes_only_processing_task_queued_and_processing() {
        let (server, connection) = new_server("").await;

        insert_queue_job(
            &connection,
            1,
            "queued",
            r#"{"processing_job_id":1}"#,
            None,
            processing_task_job_type(),
        )
        .await;
        insert_queue_job(
            &connection,
            2,
            "processing",
            r#"{"processing_job_id":2}"#,
            None,
            processing_task_job_type(),
        )
        .await;
        insert_queue_job(
            &connection,
            3,
            "completed",
            r#"{"processing_job_id":3}"#,
            None,
            processing_task_job_type(),
        )
        .await;
        insert_queue_job(
            &connection,
            4,
            "queued",
            r#"{"something_else":true}"#,
            None,
            "hyperlinked::some::other::JobType",
        )
        .await;

        let response = server.get("/admin/jobs/pending-count").await;
        response.assert_status_ok();
        let payload: PendingCountResponse = response.json();

        assert_eq!(payload.pending, 2);
        assert_eq!(payload.queued, 1);
        assert_eq!(payload.processing, 1);
    }

    #[tokio::test]
    async fn jobs_dashboard_uses_updated_queue_nav_and_badge_placeholder() {
        let (server, _) = new_server("").await;

        let page = server.get("/admin/jobs").await;
        page.assert_status_ok();
        let body = page.text();

        assert!(body.contains("href=\"/admin/jobs\""));
        assert!(body.contains("data-queue-pending-badge"));
    }
}
