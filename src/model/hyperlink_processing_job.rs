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
        HyperlinkProcessingJobKind::Og => "og",
        HyperlinkProcessingJobKind::Oembed => "oembed",
        HyperlinkProcessingJobKind::Readability => "readability",
        HyperlinkProcessingJobKind::SublinkDiscovery => "sublink_discovery",
        HyperlinkProcessingJobKind::TagClassification => "tag_classification",
    }
}

fn processing_task_job_type() -> &'static str {
    std::any::type_name::<crate::queue::ProcessingTask>()
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

    Ok(connection.execute(statement).await?.rows_affected())
}

fn now_utc() -> DateTime {
    DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, ConnectionTrait, QueryFilter};

    async fn new_connection() -> DatabaseConnection {
        let connection = crate::server::test_support::new_memory_connection().await;
        crate::server::test_support::initialize_jobs_schema(&connection).await;
        connection
            .execute_unprepared(
                r#"
                CREATE UNIQUE INDEX idx_hyperlink_processing_job_active_unique
                ON hyperlink_processing_job (hyperlink_id, kind)
                WHERE state IN ('queued', 'running')
                "#,
            )
            .await
            .expect("active unique index should initialize");
        connection
    }

    #[tokio::test]
    async fn enqueue_for_hyperlink_kind_returns_existing_active_job() {
        let connection = new_connection().await;

        let first =
            enqueue_for_hyperlink_kind(&connection, 42, HyperlinkProcessingJobKind::Snapshot, None)
                .await
                .expect("first enqueue should succeed");
        let second =
            enqueue_for_hyperlink_kind(&connection, 42, HyperlinkProcessingJobKind::Snapshot, None)
                .await
                .expect("duplicate enqueue should return existing active job");

        assert_eq!(first.id, second.id);

        let active = hyperlink_processing_job::Entity::find()
            .filter(hyperlink_processing_job::Column::HyperlinkId.eq(42))
            .filter(hyperlink_processing_job::Column::Kind.eq(HyperlinkProcessingJobKind::Snapshot))
            .filter(hyperlink_processing_job::Column::State.is_in([
                HyperlinkProcessingJobState::Queued,
                HyperlinkProcessingJobState::Running,
            ]))
            .all(&connection)
            .await
            .expect("active jobs should load");
        assert_eq!(active.len(), 1);
    }

    #[tokio::test]
    async fn enqueue_for_hyperlink_kind_creates_new_job_after_completion() {
        let connection = new_connection().await;

        let first = enqueue_for_hyperlink_kind(
            &connection,
            7,
            HyperlinkProcessingJobKind::Readability,
            None,
        )
        .await
        .expect("first enqueue should succeed");

        let mut completed: hyperlink_processing_job::ActiveModel = first.clone().into();
        completed.state = Set(HyperlinkProcessingJobState::Succeeded);
        completed.finished_at = Set(Some(now_utc()));
        completed.updated_at = Set(now_utc());
        completed
            .update(&connection)
            .await
            .expect("job should update to succeeded");

        let second = enqueue_for_hyperlink_kind(
            &connection,
            7,
            HyperlinkProcessingJobKind::Readability,
            None,
        )
        .await
        .expect("enqueue after completion should create a new row");

        assert_ne!(first.id, second.id);
        assert_eq!(second.state, HyperlinkProcessingJobState::Queued);
    }

    #[tokio::test]
    async fn enqueue_for_hyperlink_kind_is_scoped_by_kind() {
        let connection = new_connection().await;

        let snapshot =
            enqueue_for_hyperlink_kind(&connection, 88, HyperlinkProcessingJobKind::Snapshot, None)
                .await
                .expect("snapshot enqueue should succeed");
        let readability = enqueue_for_hyperlink_kind(
            &connection,
            88,
            HyperlinkProcessingJobKind::Readability,
            None,
        )
        .await
        .expect("readability enqueue should succeed");

        assert_ne!(snapshot.id, readability.id);

        let active = hyperlink_processing_job::Entity::find()
            .filter(hyperlink_processing_job::Column::HyperlinkId.eq(88))
            .filter(hyperlink_processing_job::Column::State.is_in([
                HyperlinkProcessingJobState::Queued,
                HyperlinkProcessingJobState::Running,
            ]))
            .all(&connection)
            .await
            .expect("active jobs should load");

        assert_eq!(active.len(), 2);
        assert!(
            active
                .iter()
                .any(|job| job.kind == HyperlinkProcessingJobKind::Snapshot)
        );
        assert!(
            active
                .iter()
                .any(|job| job.kind == HyperlinkProcessingJobKind::Readability)
        );
    }

    #[tokio::test]
    async fn delete_stale_active_rows_only_removes_orphaned_active_rows() {
        let connection = new_connection().await;
        crate::server::test_support::initialize_queue_jobs_schema(&connection).await;

        connection
            .execute_unprepared(
                r#"
                INSERT INTO hyperlink_processing_job (id, hyperlink_id, kind, state, error_message, queued_at, started_at, finished_at, created_at, updated_at)
                VALUES
                    (1, 1, 'snapshot', 'queued', NULL, '2026-02-27 00:00:01', NULL, NULL, '2026-02-27 00:00:01', '2026-02-27 00:00:01'),
                    (2, 2, 'snapshot', 'running', NULL, '2026-02-27 00:00:02', '2026-02-27 00:00:03', NULL, '2026-02-27 00:00:02', '2026-02-27 00:00:03'),
                    (3, 3, 'snapshot', 'succeeded', NULL, '2026-02-27 00:00:04', '2026-02-27 00:00:05', '2026-02-27 00:00:06', '2026-02-27 00:00:04', '2026-02-27 00:00:06');
                "#,
            )
            .await
            .expect("processing job seed data should insert");

        let queue_seed = format!(
            "
            INSERT INTO jobs (id, job_type, payload, status, attempts, max_attempts, available_at, locked_at, lock_token, last_error, created_at, updated_at, completed_at, first_enqueued_at, last_enqueued_at, first_started_at, last_started_at, last_finished_at, queued_ms_total, queued_ms_last, processing_ms_total, processing_ms_last)
            VALUES
                (10, '{job_type}', '{{\"processing_job_id\":1}}', 'queued', 0, 3, 0, NULL, NULL, NULL, 0, 0, NULL, NULL, NULL, NULL, NULL, NULL, 0, NULL, 0, NULL),
                (11, '{job_type}', '{{\"processing_job_id\":999}}', 'processing', 0, 3, 0, NULL, NULL, NULL, 0, 0, NULL, NULL, NULL, NULL, NULL, NULL, 0, NULL, 0, NULL);
            ",
            job_type = processing_task_job_type()
        );
        connection
            .execute_unprepared(queue_seed.trim())
            .await
            .expect("queue seed data should insert");

        let affected = delete_stale_active_rows(&connection)
            .await
            .expect("stale repair should succeed");
        assert_eq!(affected, 1);

        let stale_row = hyperlink_processing_job::Entity::find_by_id(2)
            .one(&connection)
            .await
            .expect("stale row query should succeed");
        assert!(stale_row.is_none());

        let active_row = hyperlink_processing_job::Entity::find_by_id(1)
            .one(&connection)
            .await
            .expect("active row query should succeed")
            .expect("active row should exist");
        assert_eq!(active_row.state, HyperlinkProcessingJobState::Queued);

        let completed_row = hyperlink_processing_job::Entity::find_by_id(3)
            .one(&connection)
            .await
            .expect("completed row query should succeed")
            .expect("completed row should exist");
        assert_eq!(completed_row.state, HyperlinkProcessingJobState::Succeeded);
    }
}
