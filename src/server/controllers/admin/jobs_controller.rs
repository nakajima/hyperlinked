use std::collections::{HashMap, HashSet};

use axum::{
    Router,
    extract::{Query, RawForm, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing,
};
use sailfish::Template;
use sea_orm::{
    ActiveModelTrait,
    ActiveValue::Set,
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, DbErr, EntityTrait, QueryFilter,
    QueryResult, Statement,
    entity::prelude::{DateTime, DateTimeUtc},
};
use serde::{Deserialize, Serialize};

use crate::{
    entity::{
        hyperlink,
        hyperlink_processing_job::{self, HyperlinkProcessingJobState},
    },
    model::hyperlink_processing_job as hyperlink_processing_job_model,
    queue::ProcessingTask,
    server::{
        context::Context,
        flash::{Flash, FlashName, redirect_with_flash},
        views,
    },
};

const DEFAULT_LIMIT: u64 = 50;
const MAX_LIMIT: u64 = 200;

pub fn routes() -> Router<Context> {
    Router::new()
        .route("/admin/jobs", routing::get(index))
        .route(
            "/admin/jobs/recover-orphans",
            routing::post(recover_orphans),
        )
        .route(
            "/admin/jobs/clear-failed",
            routing::post(clear_failed_selected),
        )
        .route(
            "/admin/jobs/clear-failed-all",
            routing::post(clear_failed_all),
        )
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
    Cleared,
}

impl QueueStatusFilter {
    fn from_query(value: Option<&str>) -> Self {
        match value.unwrap_or("all") {
            "queued" => Self::Queued,
            "processing" => Self::Processing,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cleared" => Self::Cleared,
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
            Self::Cleared => "cleared",
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
    cleared: i64,
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
    selectable_failed: bool,
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

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub(crate) struct QueuePendingCounts {
    pub(crate) pending: i64,
    pub(crate) queued: i64,
    pub(crate) processing: i64,
}

#[derive(Debug, Default)]
struct OrphanRecoverySummary {
    found: usize,
    marked_failed: usize,
    requeued: usize,
    requeue_errors: usize,
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

async fn recover_orphans(State(state): State<Context>, headers: HeaderMap) -> Response {
    let summary =
        match recover_orphaned_running_jobs(&state.connection, state.processing_queue.as_ref())
            .await
        {
            Ok(summary) => summary,
            Err(err) => {
                return response_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to recover orphaned jobs: {err}"),
                );
            }
        };

    redirect_with_flash(
        &headers,
        "/admin/jobs",
        FlashName::Notice,
        format!(
            "Recovered orphans: found={}, marked_failed={}, requeued={}, requeue_errors={}",
            summary.found, summary.marked_failed, summary.requeued, summary.requeue_errors
        ),
    )
}

async fn clear_failed_selected(
    State(state): State<Context>,
    headers: HeaderMap,
    RawForm(raw_form): RawForm,
) -> Response {
    let mut queue_ids = serde_urlencoded::from_bytes::<Vec<(String, String)>>(&raw_form)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(key, value)| {
            if key == "queue_id" {
                value.parse::<i64>().ok()
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    queue_ids.sort_unstable();
    queue_ids.dedup();

    if queue_ids.is_empty() {
        return redirect_with_flash(
            &headers,
            "/admin/jobs",
            FlashName::Notice,
            "No failed queue rows selected.",
        );
    }

    let cleared = match set_failed_rows_cleared_by_ids(&state.connection, &queue_ids).await {
        Ok(cleared) => cleared,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to clear selected failed rows: {err}"),
            );
        }
    };

    redirect_with_flash(
        &headers,
        "/admin/jobs",
        FlashName::Notice,
        format!(
            "Cleared {} failed queue row(s) out of {} selected.",
            cleared,
            queue_ids.len()
        ),
    )
}

async fn clear_failed_all(State(state): State<Context>, headers: HeaderMap) -> Response {
    let cleared = match set_all_failed_rows_cleared(&state.connection).await {
        Ok(cleared) => cleared,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to clear all failed rows: {err}"),
            );
        }
    };

    redirect_with_flash(
        &headers,
        "/admin/jobs",
        FlashName::Notice,
        format!("Cleared {} failed queue row(s).", cleared),
    )
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
             COALESCE(SUM(CASE WHEN j.status = 'failed' OR hpj.state = 'failed' THEN 1 ELSE 0 END), 0) AS failed,
             COALESCE(SUM(CASE WHEN j.status = 'cleared' THEN 1 ELSE 0 END), 0) AS cleared
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
        cleared: try_get_by_index::<i64>(&row, 5)?,
    })
}

pub(crate) async fn fetch_pending_queue_counts(
    connection: &DatabaseConnection,
) -> Result<QueuePendingCounts, DbErr> {
    let stats = fetch_queue_stats(connection).await?;
    Ok(QueuePendingCounts {
        pending: stats.queued + stats.processing,
        queued: stats.queued,
        processing: stats.processing,
    })
}

pub(crate) async fn set_all_queued_rows_cleared(
    connection: &DatabaseConnection,
) -> Result<u64, DbErr> {
    let now_epoch = now_epoch_seconds();
    let statement = Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "UPDATE jobs
         SET status = ?,
             updated_at = ?,
             last_finished_at = COALESCE(last_finished_at, ?)
         WHERE job_type = ?
           AND status = 'queued'"
            .to_string(),
        vec![
            "cleared".into(),
            now_epoch.into(),
            now_epoch.into(),
            processing_task_job_type().into(),
        ],
    );

    Ok(connection.execute(statement).await?.rows_affected())
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
        let selectable_failed = row.queue_status == "failed";

        rows.push(AdminJobRowView {
            queue_id: row.queue_id,
            queue_status: row.queue_status,
            selectable_failed,
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

async fn recover_orphaned_running_jobs(
    connection: &DatabaseConnection,
    queue: Option<&crate::queue::ProcessingQueue>,
) -> Result<OrphanRecoverySummary, DbErr> {
    let running_jobs = hyperlink_processing_job::Entity::find()
        .filter(hyperlink_processing_job::Column::State.eq(HyperlinkProcessingJobState::Running))
        .all(connection)
        .await?;

    let mut summary = OrphanRecoverySummary::default();
    for running_job in running_jobs {
        if has_active_queue_row_for_processing_job(connection, running_job.id).await? {
            continue;
        }

        summary.found += 1;

        let now = now_utc();
        let mut failed: hyperlink_processing_job::ActiveModel = running_job.clone().into();
        failed.state = Set(HyperlinkProcessingJobState::Failed);
        failed.finished_at = Set(Some(now));
        failed.updated_at = Set(now);
        let message = running_job
            .error_message
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                "marked failed by admin recover-orphans: orphaned running job".to_string()
            });
        failed.error_message = Set(Some(message));
        failed.update(connection).await?;
        summary.marked_failed += 1;

        match hyperlink_processing_job_model::enqueue_for_hyperlink_kind(
            connection,
            running_job.hyperlink_id,
            running_job.kind.clone(),
            queue,
        )
        .await
        {
            Ok(_) => {
                summary.requeued += 1;
            }
            Err(error) => {
                summary.requeue_errors += 1;
                tracing::error!(
                    processing_job_id = running_job.id,
                    hyperlink_id = running_job.hyperlink_id,
                    kind = ?running_job.kind,
                    error = %error,
                    "failed to requeue recovered orphaned job"
                );
            }
        }
    }

    Ok(summary)
}

async fn has_active_queue_row_for_processing_job(
    connection: &DatabaseConnection,
    processing_job_id: i32,
) -> Result<bool, DbErr> {
    let statement = Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "SELECT 1
         FROM jobs
         WHERE job_type = ?
           AND status IN ('queued', 'processing')
           AND json_valid(payload)
           AND CAST(json_extract(payload, '$.processing_job_id') AS INTEGER) = ?
         LIMIT 1"
            .to_string(),
        vec![processing_task_job_type().into(), processing_job_id.into()],
    );

    Ok(connection.query_one(statement).await?.is_some())
}

async fn set_failed_rows_cleared_by_ids(
    connection: &DatabaseConnection,
    queue_ids: &[i64],
) -> Result<u64, DbErr> {
    if queue_ids.is_empty() {
        return Ok(0);
    }

    let mut sql = String::from(
        "UPDATE jobs
         SET status = ?,
             updated_at = ?,
             last_finished_at = COALESCE(last_finished_at, ?)
         WHERE job_type = ?
           AND status = 'failed'
           AND id IN (",
    );

    for (index, _) in queue_ids.iter().enumerate() {
        if index > 0 {
            sql.push_str(", ");
        }
        sql.push('?');
    }
    sql.push(')');

    let now_epoch = now_epoch_seconds();
    let mut values = Vec::with_capacity(4 + queue_ids.len());
    values.push("cleared".into());
    values.push(now_epoch.into());
    values.push(now_epoch.into());
    values.push(processing_task_job_type().into());
    for queue_id in queue_ids {
        values.push((*queue_id).into());
    }

    let statement = Statement::from_sql_and_values(DbBackend::Sqlite, sql, values);
    Ok(connection.execute(statement).await?.rows_affected())
}

async fn set_all_failed_rows_cleared(connection: &DatabaseConnection) -> Result<u64, DbErr> {
    let now_epoch = now_epoch_seconds();
    let statement = Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "UPDATE jobs
         SET status = ?,
             updated_at = ?,
             last_finished_at = COALESCE(last_finished_at, ?)
         WHERE job_type = ?
           AND status = 'failed'"
            .to_string(),
        vec![
            "cleared".into(),
            now_epoch.into(),
            now_epoch.into(),
            processing_task_job_type().into(),
        ],
    );

    Ok(connection.execute(statement).await?.rows_affected())
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
        QueueStatusFilter::Cleared => {
            sql.push_str(" AND j.status = ?");
            values.push("cleared".into());
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

fn now_utc() -> DateTime {
    DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}

fn now_epoch_seconds() -> i64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    i64::try_from(secs).unwrap_or(i64::MAX)
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
                backup_exports: crate::server::admin_backup::AdminBackupManager::default(),
                backup_imports: crate::server::admin_import::AdminImportManager::default(),
                tag_reclassify:
                    crate::server::admin_tag_reclassify::AdminTagReclassifyManager::default(),
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

    async fn queue_status(connection: &DatabaseConnection, id: i64) -> String {
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
    async fn jobs_dashboard_status_filter_supports_cleared_rows() {
        let (server, connection) = new_server("").await;

        insert_queue_job(
            &connection,
            1,
            "failed",
            r#"{"processing_job_id":1}"#,
            Some("failed"),
            processing_task_job_type(),
        )
        .await;
        insert_queue_job(
            &connection,
            2,
            "cleared",
            r#"{"processing_job_id":2}"#,
            None,
            processing_task_job_type(),
        )
        .await;

        let page = server.get("/admin/jobs?status=cleared").await;
        page.assert_status_ok();
        let body = page.text();

        assert!(!body.contains("queue#1"));
        assert!(body.contains("queue#2"));
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
    async fn clear_failed_selected_marks_only_selected_failed_rows_cleared() {
        let (server, connection) = new_server("").await;

        insert_queue_job(
            &connection,
            1,
            "failed",
            r#"{"processing_job_id":1}"#,
            Some("failed one"),
            processing_task_job_type(),
        )
        .await;
        insert_queue_job(
            &connection,
            2,
            "failed",
            r#"{"processing_job_id":2}"#,
            Some("failed two"),
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

        let response = server
            .post("/admin/jobs/clear-failed")
            .text("queue_id=2")
            .content_type("application/x-www-form-urlencoded")
            .await;
        response.assert_status_see_other();
        response.assert_header("location", "/admin/jobs");

        assert_eq!(queue_status(&connection, 1).await, "failed");
        assert_eq!(queue_status(&connection, 2).await, "cleared");
        assert_eq!(queue_status(&connection, 3).await, "queued");
    }

    #[tokio::test]
    async fn clear_failed_all_marks_all_failed_rows_cleared() {
        let (server, connection) = new_server("").await;

        insert_queue_job(
            &connection,
            10,
            "failed",
            r#"{"processing_job_id":10}"#,
            Some("failed ten"),
            processing_task_job_type(),
        )
        .await;
        insert_queue_job(
            &connection,
            11,
            "failed",
            r#"{"processing_job_id":11}"#,
            Some("failed eleven"),
            processing_task_job_type(),
        )
        .await;
        insert_queue_job(
            &connection,
            12,
            "processing",
            r#"{"processing_job_id":12}"#,
            None,
            processing_task_job_type(),
        )
        .await;

        let response = server.post("/admin/jobs/clear-failed-all").await;
        response.assert_status_see_other();
        response.assert_header("location", "/admin/jobs");

        assert_eq!(queue_status(&connection, 10).await, "cleared");
        assert_eq!(queue_status(&connection, 11).await, "cleared");
        assert_eq!(queue_status(&connection, 12).await, "processing");
    }

    #[tokio::test]
    async fn recover_orphans_marks_running_jobs_failed_and_requeues() {
        let (server, connection) = new_server(
            r#"
                INSERT INTO hyperlink (id, title, url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Orphan', 'https://example.com/orphan', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00'),
                    (2, 'Active', 'https://example.com/active', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00');
                INSERT INTO hyperlink_processing_job (id, hyperlink_id, kind, state, error_message, queued_at, started_at, finished_at, created_at, updated_at)
                VALUES
                    (101, 1, 'readability', 'running', NULL, '2026-02-21 00:01:00', '2026-02-21 00:01:05', NULL, '2026-02-21 00:01:00', '2026-02-21 00:01:05'),
                    (102, 2, 'readability', 'running', NULL, '2026-02-21 00:02:00', '2026-02-21 00:02:05', NULL, '2026-02-21 00:02:00', '2026-02-21 00:02:05');
            "#,
        )
        .await;

        insert_queue_job(
            &connection,
            900,
            "processing",
            r#"{"processing_job_id":102}"#,
            None,
            processing_task_job_type(),
        )
        .await;

        let response = server.post("/admin/jobs/recover-orphans").await;
        response.assert_status_see_other();
        response.assert_header("location", "/admin/jobs");

        let orphan_row = connection
            .query_one(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT state FROM hyperlink_processing_job WHERE id = ?".to_string(),
                vec![101.into()],
            ))
            .await
            .expect("orphan row query should succeed")
            .expect("orphan row should exist");
        let orphan_state = orphan_row
            .try_get_by_index::<String>(0)
            .expect("state should decode");
        assert_eq!(orphan_state, "failed");

        let active_row = connection
            .query_one(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT state FROM hyperlink_processing_job WHERE id = ?".to_string(),
                vec![102.into()],
            ))
            .await
            .expect("active row query should succeed")
            .expect("active row should exist");
        let active_state = active_row
            .try_get_by_index::<String>(0)
            .expect("state should decode");
        assert_eq!(active_state, "running");

        let requeue_count_row = connection
            .query_one(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT COUNT(*) FROM hyperlink_processing_job
                 WHERE hyperlink_id = ? AND kind = 'readability' AND state = 'queued'"
                    .to_string(),
                vec![1.into()],
            ))
            .await
            .expect("requeue count query should succeed")
            .expect("requeue count row should exist");
        let requeue_count = requeue_count_row
            .try_get_by_index::<i64>(0)
            .expect("count should decode");
        assert_eq!(requeue_count, 1);
    }

    #[tokio::test]
    async fn fetch_pending_queue_counts_includes_only_processing_task_queued_and_processing() {
        let (_, connection) = new_server("").await;

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

        let payload = fetch_pending_queue_counts(&connection)
            .await
            .expect("queue pending counts should load");

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
