use super::*;
use sea_orm::{DatabaseConnection, EntityTrait};

use crate::entity::{hyperlink, hyperlink_processing_job};
use crate::test_support;

async fn new_connection() -> DatabaseConnection {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_schema(&connection).await;
    test_support::initialize_queue_jobs_schema(&connection).await;
    connection
}

async fn write_temp_file(content: &str, suffix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "hyperlinked-linkwarden-import-{suffix}-{}.json",
        std::process::id()
    ));
    tokio::fs::write(&path, content)
        .await
        .expect("temp file should be writable");
    path
}

async fn import_from_json(
    connection: &DatabaseConnection,
    content: &str,
    suffix: &str,
) -> ImportReport {
    let processing_queue = crate::queue::ProcessingQueue::connect(connection.clone())
        .await
        .expect("processing queue should initialize");
    let path = write_temp_file(content, suffix).await;
    let report = import_file(
        connection,
        &path,
        ImportFormat::Auto,
        Some(&processing_queue),
    )
    .await
    .expect("import should complete");
    tokio::fs::remove_file(path)
        .await
        .expect("temp file should be removed");
    report
}

#[tokio::test]
async fn imports_top_level_array() {
    let connection = new_connection().await;
    let report = import_from_json(
        &connection,
        r#"[
                {"title":"Example","url":"https://example.com"},
                {"name":"Rust","uri":"https://www.rust-lang.org"}
            ]"#,
        "array",
    )
    .await;

    assert_eq!(report.summary.total, 2);
    assert_eq!(report.summary.inserted, 2);
    assert_eq!(report.summary.updated, 0);
    assert_eq!(report.summary.failed, 0);

    let links = hyperlink::Entity::find()
        .all(&connection)
        .await
        .expect("links should load");
    assert_eq!(links.len(), 2);

    let jobs = hyperlink_processing_job::Entity::find()
        .all(&connection)
        .await
        .expect("jobs should load");
    assert_eq!(jobs.len(), 2);
}

#[tokio::test]
async fn imports_created_at_when_present() {
    let connection = new_connection().await;
    let report = import_from_json(
        &connection,
        r#"[
                {
                    "title": "Example",
                    "url": "https://example.com",
                    "createdAt": "2025-07-25T22:41:56.384Z"
                }
            ]"#,
        "created-at",
    )
    .await;

    assert_eq!(report.summary.total, 1);
    assert_eq!(report.summary.inserted, 1);
    assert_eq!(report.summary.updated, 0);
    assert_eq!(report.summary.failed, 0);

    let imported = hyperlink::Entity::find()
        .one(&connection)
        .await
        .expect("query should succeed")
        .expect("row should exist");
    let expected = DateTime::parse_from_str("2025-07-25T22:41:56.384", "%Y-%m-%dT%H:%M:%S%.f")
        .expect("timestamp should parse");
    assert_eq!(imported.created_at, expected);
}

#[tokio::test]
async fn imports_object_with_nested_data_links_array() {
    let connection = new_connection().await;
    let report = import_from_json(
        &connection,
        r#"{
                "data": {
                    "links": [
                        {"name":"Example","link":"https://example.com"}
                    ]
                }
            }"#,
        "nested",
    )
    .await;

    assert_eq!(report.summary.total, 1);
    assert_eq!(report.summary.inserted, 1);
    assert_eq!(report.summary.updated, 0);
    assert_eq!(report.summary.failed, 0);
}

#[tokio::test]
async fn imports_collection_links_across_multiple_collections() {
    let connection = new_connection().await;
    let report = import_from_json(
        &connection,
        r#"{
                "aiPredefinedTags": ["Compiler", "Rust"],
                "collections": [
                    {
                        "name": "One",
                        "links": [
                            {"name": "Example", "url": "https://example.com"}
                        ]
                    },
                    {
                        "name": "Two",
                        "links": [
                            {"name": "Rust", "url": "https://www.rust-lang.org"},
                            {"name": "Missing URL"}
                        ]
                    }
                ]
            }"#,
        "collections",
    )
    .await;

    assert_eq!(report.summary.total, 3);
    assert_eq!(report.summary.inserted, 2);
    assert_eq!(report.summary.updated, 0);
    assert_eq!(report.summary.failed, 1);
    assert_eq!(report.failures.len(), 1);
}

#[tokio::test]
async fn updates_existing_row_by_url() {
    let connection = new_connection().await;

    let normalized = crate::app::models::hyperlink::validate_and_normalize(HyperlinkInput {
        title: "Old".to_string(),
        url: "https://example.com".to_string(),
    })
    .await
    .expect("seed hyperlink should normalize");

    let inserted = crate::app::models::hyperlink::insert(&connection, normalized, None)
        .await
        .expect("seed row should insert");

    let report = import_from_json(
        &connection,
        r#"[
                {
                    "title":"New Title",
                    "url":"https://example.com",
                    "createdAt":"2021-06-01T12:30:45.000Z"
                }
            ]"#,
        "upsert",
    )
    .await;

    assert_eq!(report.summary.total, 1);
    assert_eq!(report.summary.inserted, 0);
    assert_eq!(report.summary.updated, 1);
    assert_eq!(report.summary.failed, 0);

    let updated = hyperlink::Entity::find_by_id(inserted.id)
        .one(&connection)
        .await
        .expect("query should succeed")
        .expect("row should exist");
    assert_eq!(updated.title, "New Title");
    let expected = DateTime::parse_from_str("2021-06-01T12:30:45.000", "%Y-%m-%dT%H:%M:%S%.f")
        .expect("timestamp should parse");
    assert_eq!(updated.created_at, expected);

    let jobs = hyperlink_processing_job::Entity::find()
        .all(&connection)
        .await
        .expect("jobs should load");
    assert_eq!(jobs.len(), 0);
}

#[tokio::test]
async fn continues_after_row_errors() {
    let connection = new_connection().await;
    let report = import_from_json(
        &connection,
        r#"[
                {"title":"Valid","url":"https://example.com"},
                {"title":"Missing URL"},
                123,
                {"title":"Second Valid","url":"https://www.rust-lang.org"}
            ]"#,
        "errors",
    )
    .await;

    assert_eq!(report.summary.total, 4);
    assert_eq!(report.summary.inserted, 2);
    assert_eq!(report.summary.updated, 0);
    assert_eq!(report.summary.failed, 2);
    assert_eq!(report.failures.len(), 2);
}
