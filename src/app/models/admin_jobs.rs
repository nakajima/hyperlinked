use std::collections::{HashMap, HashSet};

use sea_orm::{
    ActiveModelTrait,
    ActiveValue::Set,
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, DbErr, EntityTrait, QueryFilter,
    QueryResult, Statement,
    entity::prelude::{DateTime, DateTimeUtc},
};
use serde::{Deserialize, Serialize};

use crate::{
    app::models::hyperlink_processing_job as hyperlink_processing_job_model,
    entity::{
        hyperlink,
        hyperlink_processing_job::{self, HyperlinkProcessingJobKind, HyperlinkProcessingJobState},
    },
    queue::ProcessingTask,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QueueStatusFilter {
    All,
    Queued,
    Processing,
    Completed,
    Failed,
    Cleared,
}

impl QueueStatusFilter {
    pub(crate) fn from_query(value: Option<&str>) -> Self {
        match value.unwrap_or("all") {
            "queued" => Self::Queued,
            "processing" => Self::Processing,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cleared" => Self::Cleared,
            _ => Self::All,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
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
pub(crate) struct QueueStats {
    pub(crate) total: i64,
    pub(crate) queued: i64,
    pub(crate) processing: i64,
    pub(crate) completed: i64,
    pub(crate) failed: i64,
    pub(crate) cleared: i64,
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
pub(crate) struct QueueRowContext {
    pub(crate) queue_id: i64,
    pub(crate) queue_status: String,
    pub(crate) processing_job_id: Option<i32>,
    pub(crate) processing_state: Option<HyperlinkProcessingJobState>,
    pub(crate) processing_kind: Option<HyperlinkProcessingJobKind>,
    pub(crate) hyperlink_id: Option<i32>,
    pub(crate) hyperlink_title: Option<String>,
    pub(crate) hyperlink_url: Option<String>,
    pub(crate) attempts: i32,
    pub(crate) max_attempts: i32,
    pub(crate) queued_ms_total: i64,
    pub(crate) queued_ms_last: Option<i64>,
    pub(crate) processing_ms_total: i64,
    pub(crate) processing_ms_last: Option<i64>,
    pub(crate) queue_error: Option<String>,
    pub(crate) processing_error: Option<String>,
    pub(crate) payload: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub(crate) struct QueuePendingCounts {
    pub(crate) pending: i64,
    pub(crate) queued: i64,
    pub(crate) processing: i64,
}

#[derive(Debug, Default)]
pub(crate) struct OrphanRecoverySummary {
    pub(crate) found: usize,
    pub(crate) marked_failed: usize,
    pub(crate) requeued: usize,
    pub(crate) requeue_errors: usize,
}

pub(crate) async fn fetch_queue_stats(
    connection: &DatabaseConnection,
) -> Result<QueueStats, DbErr> {
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

    let Some(row) = connection.query_one_raw(statement).await? else {
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

    Ok(connection.execute_raw(statement).await?.rows_affected())
}

pub(crate) async fn fetch_filtered_total(
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
    let Some(row) = connection.query_one_raw(statement).await? else {
        return Ok(0);
    };
    try_get_by_index::<i64>(&row, 0)
}

pub(crate) async fn fetch_queue_row_contexts(
    connection: &DatabaseConnection,
    status_filter: QueueStatusFilter,
    limit: u64,
    offset: u64,
) -> Result<Vec<QueueRowContext>, DbErr> {
    let queue_rows = fetch_queue_rows(connection, status_filter, limit, offset).await?;
    enrich_queue_rows(connection, queue_rows).await
}

pub(crate) async fn recover_orphaned_running_jobs(
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

        let requeue_result = hyperlink_processing_job_model::enqueue_for_hyperlink_kind(
            connection,
            running_job.hyperlink_id,
            running_job.kind.clone(),
            queue,
        )
        .await
        .map(|_| true);

        match requeue_result {
            Ok(true) => {
                summary.requeued += 1;
            }
            Ok(false) => {}
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

pub(crate) async fn set_selected_rows_cleared_by_ids(
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
           AND status != 'cleared'
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
    Ok(connection.execute_raw(statement).await?.rows_affected())
}

pub(crate) async fn set_all_failed_rows_cleared(
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
           AND status = 'failed'"
            .to_string(),
        vec![
            "cleared".into(),
            now_epoch.into(),
            now_epoch.into(),
            processing_task_job_type().into(),
        ],
    );

    Ok(connection.execute_raw(statement).await?.rows_affected())
}

pub(crate) fn processing_task_job_type() -> &'static str {
    std::any::type_name::<ProcessingTask>()
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
    let rows = connection.query_all_raw(statement).await?;
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
) -> Result<Vec<QueueRowContext>, DbErr> {
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
        let hyperlink_id = processing_job.map(|job| job.hyperlink_id);
        let hyperlink_model = hyperlink_id.and_then(|id| hyperlinks.get(&id));

        rows.push(QueueRowContext {
            queue_id: row.queue_id,
            queue_status: row.queue_status,
            processing_job_id: row.processing_job_id,
            processing_state: processing_job.map(|job| job.state.clone()),
            processing_kind: processing_job.map(|job| job.kind.clone()),
            hyperlink_id,
            hyperlink_title: hyperlink_model.map(|link| link.title.clone()),
            hyperlink_url: hyperlink_model.map(|link| link.url.clone()),
            attempts: row.attempts,
            max_attempts: row.max_attempts,
            queued_ms_total: row.queued_ms_total,
            queued_ms_last: row.queued_ms_last,
            processing_ms_total: row.processing_ms_total,
            processing_ms_last: row.processing_ms_last,
            queue_error: row.last_error,
            processing_error: processing_job.and_then(|job| job.error_message.clone()),
            payload: row.payload,
        });
    }

    Ok(rows)
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

    Ok(connection.query_one_raw(statement).await?.is_some())
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
    use super::*;

    #[test]
    fn queue_status_filter_parses_known_values() {
        assert_eq!(QueueStatusFilter::from_query(None), QueueStatusFilter::All);
        assert_eq!(
            QueueStatusFilter::from_query(Some("processing")),
            QueueStatusFilter::Processing
        );
        assert_eq!(QueueStatusFilter::Failed.as_str(), "failed");
    }

    #[test]
    fn append_status_filter_clause_builds_failed_filter_sql() {
        let mut sql = String::from(
            "SELECT * FROM jobs j LEFT JOIN hyperlink_processing_job hpj ON 1=1 WHERE j.job_type = ?",
        );
        let mut values = vec![processing_task_job_type().into()];

        append_status_filter_clause(&mut sql, &mut values, QueueStatusFilter::Failed);

        assert!(sql.contains("AND (j.status = ? OR hpj.state = ?)"));
        assert_eq!(values.len(), 3);
    }
}
