use std::collections::HashMap;

use sea_orm::{
    ActiveModelTrait,
    ActiveValue::Set,
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, Statement,
    entity::prelude::{DateTime, DateTimeUtc},
};

use crate::entity::hyperlink_processing_job::{
    self, HyperlinkProcessingJobKind, HyperlinkProcessingJobState,
};

pub type ProcessingQueueSender = crate::queue::ProcessingQueue;
const ACTIVE_JOB_UNIQUE_INDEX_NAME: &str = "idx_hyperlink_processing_job_active_unique";
const LEGACY_REMOVED_JOB_KINDS: &[&str] = &["tag_classification", "tag_reclassify"];

pub async fn enqueue_for_hyperlink(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    processing_queue: Option<&ProcessingQueueSender>,
) -> Result<hyperlink_processing_job::Model, sea_orm::DbErr> {
    enqueue_for_hyperlink_kind(
        connection,
        hyperlink_id,
        HyperlinkProcessingJobKind::Snapshot,
        processing_queue,
    )
    .await
}

pub async fn enqueue_for_hyperlink_kind(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    kind: HyperlinkProcessingJobKind,
    processing_queue: Option<&ProcessingQueueSender>,
) -> Result<hyperlink_processing_job::Model, sea_orm::DbErr> {
    let now = now_utc();
    let model = match (hyperlink_processing_job::ActiveModel {
        hyperlink_id: Set(hyperlink_id),
        kind: Set(kind.clone()),
        state: Set(HyperlinkProcessingJobState::Queued),
        error_message: Set(None),
        queued_at: Set(now),
        started_at: Set(None),
        finished_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    })
    .insert(connection)
    .await
    {
        Ok(model) => model,
        Err(error) if is_active_job_unique_violation(&error) => {
            if let Some(existing) =
                active_for_hyperlink_kind(connection, hyperlink_id, kind).await?
            {
                return Ok(existing);
            }
            return Err(error);
        }
        Err(error) => return Err(error),
    };

    if let Some(queue) = processing_queue {
        if let Err(error) = queue.enqueue_processing_job(model.id).await {
            if let Err(cleanup_error) = hyperlink_processing_job::Entity::delete_by_id(model.id)
                .exec(connection)
                .await
            {
                tracing::error!(
                    hyperlink_id,
                    job_id = model.id,
                    enqueue_error = %error,
                    cleanup_error = %cleanup_error,
                    "failed to enqueue hyperlink processing job and failed to clean up queued row"
                );
            }
            return Err(sea_orm::DbErr::Custom(
                format!(
                    "failed to enqueue hyperlink processing job {}: {error}",
                    model.id
                )
                .into(),
            ));
        }
    }

    Ok(model)
}

async fn active_for_hyperlink_kind(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    kind: HyperlinkProcessingJobKind,
) -> Result<Option<hyperlink_processing_job::Model>, sea_orm::DbErr> {
    hyperlink_processing_job::Entity::find()
        .filter(hyperlink_processing_job::Column::HyperlinkId.eq(hyperlink_id))
        .filter(hyperlink_processing_job::Column::Kind.eq(kind))
        .filter(hyperlink_processing_job::Column::State.is_in([
            HyperlinkProcessingJobState::Queued,
            HyperlinkProcessingJobState::Running,
        ]))
        .order_by_desc(hyperlink_processing_job::Column::CreatedAt)
        .order_by_desc(hyperlink_processing_job::Column::Id)
        .one(connection)
        .await
}

fn is_active_job_unique_violation(error: &sea_orm::DbErr) -> bool {
    let message = match error {
        sea_orm::DbErr::Exec(exec_error) => exec_error.to_string(),
        sea_orm::DbErr::Query(query_error) => query_error.to_string(),
        _ => return false,
    };

    let normalized = message.to_ascii_lowercase();
    normalized.contains(ACTIVE_JOB_UNIQUE_INDEX_NAME)
        || normalized.contains(
            "unique constraint failed: hyperlink_processing_job.hyperlink_id, hyperlink_processing_job.kind",
        )
}

pub async fn latest_for_hyperlink(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
) -> Result<Option<hyperlink_processing_job::Model>, sea_orm::DbErr> {
    hyperlink_processing_job::Entity::find()
        .filter(hyperlink_processing_job::Column::HyperlinkId.eq(hyperlink_id))
        .filter(hyperlink_processing_job::Column::Kind.is_in(supported_kinds()))
        .order_by_desc(hyperlink_processing_job::Column::CreatedAt)
        .order_by_desc(hyperlink_processing_job::Column::Id)
        .one(connection)
        .await
}

pub async fn latest_for_hyperlinks(
    connection: &DatabaseConnection,
    hyperlink_ids: &[i32],
) -> Result<HashMap<i32, hyperlink_processing_job::Model>, sea_orm::DbErr> {
    if hyperlink_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let jobs = hyperlink_processing_job::Entity::find()
        .filter(hyperlink_processing_job::Column::HyperlinkId.is_in(hyperlink_ids.to_vec()))
        .filter(hyperlink_processing_job::Column::Kind.is_in(supported_kinds()))
        .order_by_desc(hyperlink_processing_job::Column::CreatedAt)
        .order_by_desc(hyperlink_processing_job::Column::Id)
        .all(connection)
        .await?;

    let mut latest = HashMap::with_capacity(hyperlink_ids.len());
    for job in jobs {
        latest.entry(job.hyperlink_id).or_insert(job);
    }

    Ok(latest)
}

pub async fn recent_for_hyperlink(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    limit: u64,
) -> Result<Vec<hyperlink_processing_job::Model>, sea_orm::DbErr> {
    hyperlink_processing_job::Entity::find()
        .filter(hyperlink_processing_job::Column::HyperlinkId.eq(hyperlink_id))
        .filter(hyperlink_processing_job::Column::Kind.is_in(supported_kinds()))
        .order_by_desc(hyperlink_processing_job::Column::CreatedAt)
        .order_by_desc(hyperlink_processing_job::Column::Id)
        .limit(limit)
        .all(connection)
        .await
}

pub async fn delete_removed_job_rows(
    connection: &DatabaseConnection,
) -> Result<u64, sea_orm::DbErr> {
    let placeholders = LEGACY_REMOVED_JOB_KINDS
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let statement = Statement::from_sql_and_values(
        DbBackend::Sqlite,
        format!("DELETE FROM hyperlink_processing_job WHERE kind IN ({placeholders})"),
        LEGACY_REMOVED_JOB_KINDS
            .iter()
            .map(|kind| (*kind).into())
            .collect::<Vec<_>>(),
    );

    Ok(connection.execute_raw(statement).await?.rows_affected())
}

pub fn state_name(state: HyperlinkProcessingJobState) -> &'static str {
    match state {
        HyperlinkProcessingJobState::Queued => "queued",
        HyperlinkProcessingJobState::Running => "running",
        HyperlinkProcessingJobState::Succeeded => "succeeded",
        HyperlinkProcessingJobState::Failed => "failed",
    }
}

pub fn kind_name(kind: HyperlinkProcessingJobKind) -> &'static str {
    match kind {
        HyperlinkProcessingJobKind::Snapshot => "snapshot",
        HyperlinkProcessingJobKind::Og => "og",
        HyperlinkProcessingJobKind::Oembed => "oembed",
        HyperlinkProcessingJobKind::Readability => "readability",
        HyperlinkProcessingJobKind::SublinkDiscovery => "sublink_discovery",
    }
}

fn processing_task_job_type() -> &'static str {
    std::any::type_name::<crate::queue::ProcessingTask>()
}

fn supported_kinds() -> [HyperlinkProcessingJobKind; 5] {
    [
        HyperlinkProcessingJobKind::Snapshot,
        HyperlinkProcessingJobKind::Og,
        HyperlinkProcessingJobKind::Oembed,
        HyperlinkProcessingJobKind::Readability,
        HyperlinkProcessingJobKind::SublinkDiscovery,
    ]
}

pub async fn delete_stale_active_rows(
    connection: &DatabaseConnection,
) -> Result<u64, sea_orm::DbErr> {
    let statement = Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "DELETE FROM hyperlink_processing_job
         WHERE state IN ('queued', 'running')
           AND NOT EXISTS (
               SELECT 1
               FROM jobs queue_job
               WHERE queue_job.job_type = ?
                 AND queue_job.status IN ('queued', 'processing')
                 AND json_valid(queue_job.payload)
                 AND CAST(json_extract(queue_job.payload, '$.processing_job_id') AS INTEGER) = hyperlink_processing_job.id
           )"
            .to_string(),
        vec![processing_task_job_type().into()],
    );

    Ok(connection.execute_raw(statement).await?.rows_affected())
}

fn now_utc() -> DateTime {
    DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}
#[cfg(test)]
#[path = "../../../tests/unit/app_models_hyperlink_processing_job.rs"]
mod tests;
