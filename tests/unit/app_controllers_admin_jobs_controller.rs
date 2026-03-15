use axum::Router;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};

use super::*;
use crate::test_support;

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
    assert!(body.contains("Worker concurrency"));
    assert!(body.contains("N/A"));
}

#[tokio::test]
async fn jobs_dashboard_renders_worker_concurrency_when_queue_is_available() {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_schema(&connection).await;
    test_support::initialize_queue_jobs_schema(&connection).await;

    let queue = crate::queue::ProcessingQueue::connect(connection.clone())
        .await
        .expect("processing queue should initialize");
    let expected_concurrency = queue
        .dashboard_runtime_state()
        .expect("queue runtime state should load")
        .configured_concurrency;

    let app = Router::<Context>::new()
        .merge(routes())
        .with_state(Context {
            connection,
            processing_queue: Some(queue),
            backup_exports: crate::server::admin_backup::AdminBackupManager::default(),
            backup_imports: crate::server::admin_import::AdminImportManager::default(),
        });
    let server = axum_test::TestServer::new(app).expect("test server should initialize");

    let page = server.get("/admin/jobs").await;
    page.assert_status_ok();
    let body = page.text();

    assert!(body.contains("Worker concurrency"));
    let marker = "Worker concurrency:</span>";
    let marker_index = body.find(marker).expect("concurrency marker should exist");
    let worker_section = &body[marker_index..];
    assert!(
        worker_section.contains(&format!("<span>{expected_concurrency}</span>")),
        "expected worker concurrency value {expected_concurrency} in page body"
    );
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
async fn clear_selected_marks_only_selected_rows_cleared() {
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
        .post("/admin/jobs/clear-selected")
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
async fn clear_selected_can_clear_processing_and_completed_rows() {
    let (server, connection) = new_server("").await;

    insert_queue_job(
        &connection,
        21,
        "processing",
        r#"{"processing_job_id":21}"#,
        None,
        processing_task_job_type(),
    )
    .await;
    insert_queue_job(
        &connection,
        22,
        "completed",
        r#"{"processing_job_id":22}"#,
        None,
        processing_task_job_type(),
    )
    .await;
    insert_queue_job(
        &connection,
        23,
        "cleared",
        r#"{"processing_job_id":23}"#,
        None,
        processing_task_job_type(),
    )
    .await;

    let response = server
        .post("/admin/jobs/clear-selected")
        .text("queue_id=21&queue_id=22&queue_id=23")
        .content_type("application/x-www-form-urlencoded")
        .await;
    response.assert_status_see_other();
    response.assert_header("location", "/admin/jobs");

    assert_eq!(queue_status(&connection, 21).await, "cleared");
    assert_eq!(queue_status(&connection, 22).await, "cleared");
    assert_eq!(queue_status(&connection, 23).await, "cleared");
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
async fn jobs_dashboard_layout_uses_admin_link_without_queue_badge_placeholder() {
    let (server, _) = new_server("").await;

    let page = server.get("/admin/jobs").await;
    page.assert_status_ok();
    let body = page.text();

    assert!(body.contains("href=\"/admin/artifacts\""));
    assert!(!body.contains("data-queue-pending-badge"));
}
