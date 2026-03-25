use super::*;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, ConnectionTrait, QueryFilter};

async fn new_connection() -> DatabaseConnection {
    let connection = crate::test_support::new_memory_connection().await;
    crate::test_support::initialize_jobs_schema(&connection).await;
    connection
        .execute_unprepared(
            r#"
                INSERT INTO hyperlink (id, title, url, created_at, updated_at)
                VALUES
                    (1, 'Link 1', 'https://example.com/1', '2026-03-14 00:00:00', '2026-03-14 00:00:00'),
                    (2, 'Link 2', 'https://example.com/2', '2026-03-14 00:00:00', '2026-03-14 00:00:00'),
                    (3, 'Link 3', 'https://example.com/3', '2026-03-14 00:00:00', '2026-03-14 00:00:00'),
                    (5, 'Link 5', 'https://example.com/5', '2026-03-14 00:00:00', '2026-03-14 00:00:00'),
                    (7, 'Link 7', 'https://example.com/7', '2026-03-14 00:00:00', '2026-03-14 00:00:00'),
                    (8, 'Link 8', 'https://example.com/8', '2026-03-14 00:00:00', '2026-03-14 00:00:00'),
                    (42, 'Link 42', 'https://example.com/42', '2026-03-14 00:00:00', '2026-03-14 00:00:00'),
                    (88, 'Link 88', 'https://example.com/88', '2026-03-14 00:00:00', '2026-03-14 00:00:00');
            "#,
        )
        .await
        .expect("hyperlink seed rows should insert");
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
    crate::test_support::initialize_queue_jobs_schema(&connection).await;

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

