use std::collections::HashMap;

use sea_orm::{
    ActiveModelTrait,
    ActiveValue::Set,
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    entity::prelude::{DateTime, DateTimeUtc},
};

use crate::entity::hyperlink_processing_job::{
    self, HyperlinkProcessingJobKind, HyperlinkProcessingJobState,
};

pub type ProcessingQueueSender = crate::queue::ProcessingQueue;

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
    let model = hyperlink_processing_job::ActiveModel {
        hyperlink_id: Set(hyperlink_id),
        kind: Set(kind),
        state: Set(HyperlinkProcessingJobState::Queued),
        error_message: Set(None),
        queued_at: Set(now),
        started_at: Set(None),
        finished_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(connection)
    .await?;

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

pub async fn latest_for_hyperlink(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
) -> Result<Option<hyperlink_processing_job::Model>, sea_orm::DbErr> {
    hyperlink_processing_job::Entity::find()
        .filter(hyperlink_processing_job::Column::HyperlinkId.eq(hyperlink_id))
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
        .order_by_desc(hyperlink_processing_job::Column::CreatedAt)
        .order_by_desc(hyperlink_processing_job::Column::Id)
        .limit(limit)
        .all(connection)
        .await
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
        HyperlinkProcessingJobKind::Readability => "readability",
        HyperlinkProcessingJobKind::SublinkDiscovery => "sublink_discovery",
    }
}

fn now_utc() -> DateTime {
    DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}
