use axum::{
    Router,
    extract::{Query, RawForm, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing,
};
use sailfish::Template;
use serde::Deserialize;

use crate::{
    app::{
        helpers::admin_jobs::{
            AdminJobRowView, build_admin_job_row_views, jobs_href, page_offset, range_end,
            range_start, total_pages,
        },
        models::admin_jobs::{
            QueueStats, QueueStatusFilter, fetch_filtered_total, fetch_queue_row_contexts,
            fetch_queue_stats, recover_orphaned_running_jobs, set_all_failed_rows_cleared,
            set_selected_rows_cleared_by_ids,
        },
    },
    server::{
        context::Context,
        flash::{Flash, FlashName, redirect_with_flash},
        views,
    },
};

pub(crate) use crate::app::models::admin_jobs::{
    QueuePendingCounts, fetch_pending_queue_counts, set_all_queued_rows_cleared,
};

#[cfg(test)]
pub(crate) use crate::app::models::admin_jobs::processing_task_job_type;

const DEFAULT_LIMIT: u64 = 50;
const MAX_LIMIT: u64 = 200;

pub fn routes() -> Router<Context> {
    Router::new()
        .route("/admin/jobs", routing::get(index))
        .route(
            "/admin/jobs/recover-orphans",
            routing::post(recover_orphans),
        )
        .route("/admin/jobs/clear-selected", routing::post(clear_selected))
        .route("/admin/jobs/clear-failed", routing::post(clear_selected))
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

#[derive(Template)]
#[template(path = "admin/jobs.stpl")]
struct AdminJobsTemplate<'a> {
    stats: &'a QueueStats,
    rows: &'a [AdminJobRowView],
    worker_concurrency: Option<usize>,
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

    let row_contexts =
        match fetch_queue_row_contexts(&state.connection, status_filter, limit, offset).await {
            Ok(rows) => rows,
            Err(err) => {
                return response_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to load queue rows: {err}"),
                );
            }
        };
    let rows = build_admin_job_row_views(row_contexts);
    let worker_concurrency = configured_worker_concurrency(state.processing_queue.as_ref());

    views::render_html_page_with_flash(
        "Jobs",
        render_index(
            &stats,
            &rows,
            worker_concurrency,
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

async fn clear_selected(
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
            "No queue rows selected.",
        );
    }

    let cleared = match set_selected_rows_cleared_by_ids(&state.connection, &queue_ids).await {
        Ok(cleared) => cleared,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to clear selected queue rows: {err}"),
            );
        }
    };

    redirect_with_flash(
        &headers,
        "/admin/jobs",
        FlashName::Notice,
        format!(
            "Cleared {} queue row(s) out of {} selected.",
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
    worker_concurrency: Option<usize>,
    status_filter: QueueStatusFilter,
    limit: u64,
    page: u64,
    total_pages: u64,
    total_rows: i64,
) -> Result<String, sailfish::RenderError> {
    let range_start = range_start(total_rows, page, limit);
    let range_end = range_end(range_start, rows.len());

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
        worker_concurrency,
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

fn configured_worker_concurrency(queue: Option<&crate::queue::ProcessingQueue>) -> Option<usize> {
    let queue = queue?;
    match queue.dashboard_runtime_state() {
        Ok(runtime_state) => Some(runtime_state.configured_concurrency),
        Err(error) => {
            tracing::warn!(
                error = %error,
                "failed to load queue runtime state for admin jobs page"
            );
            None
        }
    }
}

fn response_error(status: StatusCode, message: impl Into<String>) -> Response {
    views::render_error_page(status, message, "/admin/jobs", "Back to jobs dashboard")
}

fn resolve_limit(limit: Option<u64>) -> u64 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

fn resolve_page(page: Option<u64>) -> u64 {
    page.unwrap_or(1).max(1)
}

#[cfg(test)]
#[path = "../../../../tests/unit/app_controllers_admin_jobs_controller.rs"]
mod tests;
