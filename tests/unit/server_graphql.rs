use super::*;
use crate::test_support;
use axum_test::TestServer;
use serde_json::{Value, json};

async fn new_server() -> TestServer {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_schema(&connection).await;
    test_support::execute_sql(
            &connection,
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Example', 'https://example.com', 'https://example.com?utm_source=newsletter', 0, 2, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Discovered Child', 'https://example.com/child', 'https://example.com/child', 1, 0, NULL, '2026-02-19 00:00:10', '2026-02-19 00:00:10');
                INSERT INTO hyperlink_processing_job (id, hyperlink_id, kind, state, error_message, queued_at, started_at, finished_at, created_at, updated_at)
                VALUES (1, 1, 'snapshot', 'queued', NULL, '2026-02-19 00:00:01', NULL, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES (1, 1, 1, 'snapshot_warc', x'01AB', 'application/warc', 2, '2026-02-19 00:00:02'),
                       (2, 1, 1, 'pdf_source', x'25504446', 'application/pdf', 4, '2026-02-19 00:00:03');
                INSERT INTO hyperlink_relation (id, parent_hyperlink_id, child_hyperlink_id, created_at)
                VALUES (1, 1, 2, '2026-02-19 00:00:11');
                INSERT INTO hyperlink_tombstone (hyperlink_id, updated_at)
                VALUES (9, '2026-02-19 00:00:20');
            "#,
        )
        .await;

    let app = Router::<Context>::new()
        .merge(routes())
        .with_state(Context {
            connection,
            processing_queue: None,
            backup_exports: crate::server::admin_backup::AdminBackupManager::default(),
            backup_imports: crate::server::admin_import::AdminImportManager::default(),
        });

    TestServer::new(app).expect("test server should initialize")
}

async fn run_graphql(server: &TestServer, query: &str) -> Value {
    let response = server
        .post("/graphql")
        .json(&json!({ "query": query }))
        .await;
    response.assert_status_ok();
    response.json()
}

#[tokio::test]
async fn graphql_query_uses_seaography_connection_shape() {
    let server = new_server().await;
    let payload = run_graphql(
        &server,
        r#"
            {
              hyperlinks(
                filters: { discoveryDepth: { eq: 0 } }
                pagination: { page: { limit: 10, page: 0 } }
                orderBy: { id: ASC }
              ) {
                nodes {
                  id
                  title
                  url
                  rawUrl
                  discoveryDepth
                  thumbnailUrl
                  thumbnailDarkUrl
                  screenshotUrl
                  screenshotDarkUrl
                  sublinks(
                    pagination: { page: { limit: 10, page: 0 } }
                    orderBy: { id: ASC }
                  ) {
                    nodes { id url }
                  }
                  hyperlinkProcessingJob(
                    pagination: { page: { limit: 10, page: 0 } }
                    orderBy: { id: ASC }
                  ) {
                    nodes { kind state }
                  }
                }
              }
            }
            "#,
    )
    .await;

    assert_eq!(
        payload["data"]["hyperlinks"]["nodes"][0]["title"],
        "Example"
    );
    assert_eq!(
        payload["data"]["hyperlinks"]["nodes"][0]["rawUrl"],
        "https://example.com?utm_source=newsletter"
    );
    assert_eq!(
        payload["data"]["hyperlinks"]["nodes"][0]["discoveryDepth"],
        0
    );
    assert!(
        payload["data"]["hyperlinks"]["nodes"][0]["thumbnailUrl"]
            .as_str()
            .unwrap_or("")
            .ends_with("/hyperlinks/1/artifacts/screenshot_thumb_webp/inline")
    );
    assert!(
        payload["data"]["hyperlinks"]["nodes"][0]["thumbnailDarkUrl"]
            .as_str()
            .unwrap_or("")
            .ends_with("/hyperlinks/1/artifacts/screenshot_thumb_dark_webp/inline")
    );
    assert!(
        payload["data"]["hyperlinks"]["nodes"][0]["screenshotUrl"]
            .as_str()
            .unwrap_or("")
            .ends_with("/hyperlinks/1/artifacts/screenshot_webp/inline")
    );
    assert!(
        payload["data"]["hyperlinks"]["nodes"][0]["screenshotDarkUrl"]
            .as_str()
            .unwrap_or("")
            .ends_with("/hyperlinks/1/artifacts/screenshot_dark_webp/inline")
    );
    assert_eq!(
        payload["data"]["hyperlinks"]["nodes"][0]["sublinks"]["nodes"][0]["id"],
        2
    );
    assert_eq!(
        payload["data"]["hyperlinks"]["nodes"][0]["sublinks"]["nodes"][0]["url"],
        "https://example.com/child"
    );
    assert_eq!(
        payload["data"]["hyperlinks"]["nodes"]
            .as_array()
            .expect("nodes should be an array")
            .len(),
        1
    );
    assert_eq!(
        payload["data"]["hyperlinks"]["nodes"][0]["hyperlinkProcessingJob"]["nodes"][0]["state"],
        "queued"
    );
}

#[tokio::test]
async fn graphql_hyperlinks_supports_q_argument() {
    let server = new_server().await;
    let payload = run_graphql(
        &server,
        r#"
            {
              hyperlinks(
                q: "with:discovered status:idle"
                pagination: { page: { limit: 10, page: 0 } }
              ) {
                nodes { id title discoveryDepth }
              }
            }
            "#,
    )
    .await;

    let nodes = payload["data"]["hyperlinks"]["nodes"]
        .as_array()
        .expect("nodes should be an array");
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["id"], 2);
    assert_eq!(nodes[0]["title"], "Discovered Child");
    assert_eq!(nodes[0]["discoveryDepth"], 1);
}

#[tokio::test]
async fn graphql_hyperlink_exposes_discovered_via() {
    let server = new_server().await;
    let payload = run_graphql(
        &server,
        r#"
            {
              hyperlinks(
                filters: { id: { eq: 2 } }
                pagination: { page: { limit: 10, page: 0 } }
              ) {
                nodes {
                  id
                  discoveredVia {
                    id
                    title
                    url
                    rawUrl
                  }
                }
              }
            }
            "#,
    )
    .await;

    let discovered_via = payload["data"]["hyperlinks"]["nodes"][0]["discoveredVia"]
        .as_array()
        .expect("discoveredVia should be an array");
    assert_eq!(discovered_via.len(), 1);
    assert_eq!(discovered_via[0]["id"], 1);
    assert_eq!(discovered_via[0]["title"], "Example");
    assert_eq!(discovered_via[0]["url"], "https://example.com");
    assert_eq!(
        discovered_via[0]["rawUrl"],
        "https://example.com?utm_source=newsletter"
    );
}

#[tokio::test]
async fn graphql_hyperlinks_allows_null_q_argument() {
    let server = new_server().await;
    let payload = run_graphql(
        &server,
        r#"
            {
              hyperlinks(
                q: null
                pagination: { page: { limit: 10, page: 0 } }
                orderBy: { id: ASC }
              ) {
                nodes { id }
              }
            }
            "#,
    )
    .await;

    let nodes = payload["data"]["hyperlinks"]["nodes"]
        .as_array()
        .expect("nodes should be an array");
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes[0]["id"], 1);
    assert_eq!(nodes[1]["id"], 2);
}

#[tokio::test]
async fn graphql_updated_hyperlinks_returns_updates_and_tombstones() {
    let server = new_server().await;
    let payload = run_graphql(
        &server,
        r#"
            {
              updatedHyperlinks(updatedAt: "2026-02-19T00:00:05Z") {
                serverUpdatedAt
                changes {
                  id
                  changeType
                  updatedAt
                  hyperlink { id title }
                }
              }
            }
            "#,
    )
    .await;

    assert_eq!(
        payload["data"]["updatedHyperlinks"]["serverUpdatedAt"],
        "2026-02-19T00:00:20.000Z"
    );

    let changes = payload["data"]["updatedHyperlinks"]["changes"]
        .as_array()
        .expect("changes should be an array");
    assert_eq!(changes.len(), 2);

    assert_eq!(changes[0]["id"], 2);
    assert_eq!(changes[0]["changeType"], "UPDATED");
    assert_eq!(changes[0]["updatedAt"], "2026-02-19T00:00:10.000Z");
    assert_eq!(changes[0]["hyperlink"]["id"], 2);
    assert_eq!(changes[0]["hyperlink"]["title"], "Discovered Child");

    assert_eq!(changes[1]["id"], 9);
    assert_eq!(changes[1]["changeType"], "DELETED");
    assert_eq!(changes[1]["updatedAt"], "2026-02-19T00:00:20.000Z");
    assert!(changes[1]["hyperlink"].is_null());
}

#[tokio::test]
async fn graphql_updated_hyperlinks_rejects_invalid_updated_at() {
    let server = new_server().await;
    let payload = run_graphql(
        &server,
        r#"
            {
              updatedHyperlinks(updatedAt: "not-a-date") {
                serverUpdatedAt
              }
            }
            "#,
    )
    .await;

    let errors = payload["errors"].as_array().expect("errors should exist");
    assert!(
        !errors.is_empty(),
        "invalid updatedAt should return a graphql error"
    );

    let first_error_message = errors
        .first()
        .and_then(|item| item.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    assert!(
        first_error_message.contains("updatedat must be rfc3339 timestamp"),
        "expected validator error, got: {first_error_message}"
    );
}

#[tokio::test]
async fn graphql_query_artifacts_connection_works() {
    let server = new_server().await;
    let payload = run_graphql(
        &server,
        r#"
            {
              hyperlinkArtifact(
                pagination: { page: { limit: 10, page: 0 } }
                orderBy: { id: ASC }
              ) {
                nodes { kind contentType }
              }
            }
            "#,
    )
    .await;

    assert_eq!(
        payload["data"]["hyperlinkArtifact"]["nodes"]
            .as_array()
            .expect("nodes should be an array")
            .len(),
        2
    );
    assert_eq!(
        payload["data"]["hyperlinkArtifact"]["nodes"][1]["contentType"],
        "application/pdf"
    );
}

#[tokio::test]
async fn graphql_mutation_is_rejected() {
    let server = new_server().await;
    let payload = run_graphql(&server, "mutation { _ping }").await;
    let errors = payload["errors"].as_array().expect("errors should exist");
    assert!(
        !errors.is_empty(),
        "mutation should fail on read-only schema"
    );
    let first_error_message = errors
        .first()
        .and_then(|item| item.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    assert!(
        first_error_message.contains("mutations are disabled"),
        "expected read-only error, got: {first_error_message}"
    );
}

#[tokio::test]
async fn graphiql_is_available() {
    let server = new_server().await;
    let page = server.get("/graphql").await;
    page.assert_status_ok();
    assert!(page.text().contains("GraphiQL"));
}
