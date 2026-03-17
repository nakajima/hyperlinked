use crate::app::models::{
    admin_jobs::{QueueRowContext, QueueStatusFilter},
    hyperlink_processing_job as hyperlink_processing_job_model,
};

#[derive(Clone, Debug)]
pub(crate) struct AdminJobRowView {
    pub(crate) queue_id: i64,
    pub(crate) queue_status: String,
    pub(crate) selectable_for_clear: bool,
    pub(crate) processing_job_id: Option<i32>,
    pub(crate) processing_state: Option<String>,
    pub(crate) processing_kind: Option<String>,
    pub(crate) hyperlink_id: Option<i32>,
    pub(crate) hyperlink_title: Option<String>,
    pub(crate) hyperlink_url: Option<String>,
    pub(crate) attempts_display: String,
    pub(crate) queued_timing_display: String,
    pub(crate) processing_timing_display: String,
    pub(crate) error_display: Option<String>,
    pub(crate) payload_excerpt: String,
    pub(crate) payload_unmapped: bool,
}

pub(crate) fn build_admin_job_row_views(rows: Vec<QueueRowContext>) -> Vec<AdminJobRowView> {
    rows.into_iter()
        .map(|row| {
            let selectable_for_clear = row.queue_status != "cleared";
            let error_display = match (row.processing_error.as_deref(), row.queue_error.as_deref())
            {
                (Some(processing), Some(queue)) if processing == queue => {
                    Some(processing.to_string())
                }
                (Some(processing), Some(queue)) => Some(format!("{processing} (queue: {queue})")),
                (Some(processing), None) => Some(processing.to_string()),
                (None, Some(queue)) => Some(queue.to_string()),
                (None, None) => None,
            };

            AdminJobRowView {
                queue_id: row.queue_id,
                queue_status: row.queue_status,
                selectable_for_clear,
                processing_job_id: row.processing_job_id,
                processing_state: row
                    .processing_state
                    .map(|state| hyperlink_processing_job_model::state_name(state).to_string()),
                processing_kind: row
                    .processing_kind
                    .map(|kind| hyperlink_processing_job_model::kind_name(kind).to_string()),
                hyperlink_id: row.hyperlink_id,
                hyperlink_title: row.hyperlink_title,
                hyperlink_url: row.hyperlink_url,
                attempts_display: format!("{}/{}", row.attempts, row.max_attempts),
                queued_timing_display: format_timing(row.queued_ms_total, row.queued_ms_last),
                processing_timing_display: format_timing(
                    row.processing_ms_total,
                    row.processing_ms_last,
                ),
                error_display,
                payload_excerpt: truncate(&row.payload, 120),
                payload_unmapped: row.processing_job_id.is_none(),
            }
        })
        .collect()
}

pub(crate) fn total_pages(total_rows: i64, limit: u64) -> u64 {
    if total_rows <= 0 {
        return 1;
    }

    let total_rows_u64 = u64::try_from(total_rows).unwrap_or(u64::MAX);
    total_rows_u64.div_ceil(limit.max(1))
}

pub(crate) fn page_offset(page: u64, limit: u64) -> u64 {
    page.saturating_sub(1).saturating_mul(limit.max(1))
}

pub(crate) fn range_start(total_rows: i64, page: u64, limit: u64) -> i64 {
    if total_rows == 0 {
        0
    } else {
        i64::try_from(page_offset(page, limit)).unwrap_or(i64::MAX) + 1
    }
}

pub(crate) fn range_end(range_start: i64, row_count: usize) -> i64 {
    if row_count == 0 {
        0
    } else {
        range_start + i64::try_from(row_count).unwrap_or(0) - 1
    }
}

pub(crate) fn jobs_href(status_filter: QueueStatusFilter, limit: u64, page: u64) -> String {
    format!(
        "/admin/jobs?status={}&limit={}&page={}",
        status_filter.as_str(),
        limit,
        page
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::hyperlink_processing_job::{
        HyperlinkProcessingJobKind, HyperlinkProcessingJobState,
    };

    #[test]
    fn pagination_helpers_compute_expected_ranges() {
        assert_eq!(total_pages(0, 50), 1);
        assert_eq!(total_pages(101, 50), 3);
        assert_eq!(page_offset(3, 50), 100);
        assert_eq!(range_start(101, 3, 50), 101);
        assert_eq!(range_end(101, 1), 101);
        assert_eq!(
            jobs_href(QueueStatusFilter::Failed, 50, 2),
            "/admin/jobs?status=failed&limit=50&page=2"
        );
    }

    #[test]
    fn row_views_format_errors_and_mark_unmapped_payloads() {
        let rows = build_admin_job_row_views(vec![
            QueueRowContext {
                queue_id: 7,
                queue_status: "failed".to_string(),
                processing_job_id: Some(41),
                processing_state: Some(HyperlinkProcessingJobState::Failed),
                processing_kind: Some(HyperlinkProcessingJobKind::Snapshot),
                hyperlink_id: Some(12),
                hyperlink_title: Some("Example".to_string()),
                hyperlink_url: Some("https://example.com".to_string()),
                attempts: 2,
                max_attempts: 5,
                queued_ms_total: 40,
                queued_ms_last: Some(10),
                processing_ms_total: 80,
                processing_ms_last: Some(20),
                queue_error: Some("queue failure".to_string()),
                processing_error: Some("processor failure".to_string()),
                payload: "{\"processing_job_id\":41}".to_string(),
            },
            QueueRowContext {
                queue_id: 8,
                queue_status: "cleared".to_string(),
                processing_job_id: None,
                processing_state: None,
                processing_kind: None,
                hyperlink_id: None,
                hyperlink_title: None,
                hyperlink_url: None,
                attempts: 1,
                max_attempts: 3,
                queued_ms_total: 5,
                queued_ms_last: None,
                processing_ms_total: 0,
                processing_ms_last: None,
                queue_error: None,
                processing_error: None,
                payload: "{\"unexpected\":true}".to_string(),
            },
        ]);

        assert_eq!(
            rows[0].error_display.as_deref(),
            Some("processor failure (queue: queue failure)")
        );
        assert_eq!(rows[0].processing_state.as_deref(), Some("failed"));
        assert_eq!(rows[0].processing_kind.as_deref(), Some("snapshot"));
        assert_eq!(rows[0].attempts_display, "2/5");
        assert_eq!(rows[0].queued_timing_display, "40 ms total | 10 ms last");
        assert!(!rows[0].payload_unmapped);

        assert!(!rows[1].selectable_for_clear);
        assert!(rows[1].payload_unmapped);
        assert_eq!(rows[1].payload_excerpt, "{\"unexpected\":true}");
    }
}
