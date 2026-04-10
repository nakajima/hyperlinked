use super::*;
use crate::test_support;
use axum_test::TestServer;
use sea_orm::DatabaseConnection;
use serde::Serialize;
use serde_json::json;

async fn new_server() -> TestServer {
    new_server_with_seed(None).await
}

async fn new_server_with_seed(seed_sql: Option<&str>) -> TestServer {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_schema_with_search(&connection).await;
    test_support::initialize_queue_jobs_schema(&connection).await;
    if let Some(seed_sql) = seed_sql {
        test_support::execute_sql(&connection, seed_sql).await;
    }

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

async fn new_server_with_queue(seed_sql: Option<&str>) -> (TestServer, DatabaseConnection) {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_schema_with_search(&connection).await;
    test_support::initialize_queue_jobs_schema(&connection).await;
    if let Some(seed_sql) = seed_sql {
        test_support::execute_sql(&connection, seed_sql).await;
    }

    let queue = crate::queue::ProcessingQueue::connect(connection.clone())
        .await
        .expect("processing queue should initialize");
    let app = Router::<Context>::new()
        .merge(routes())
        .with_state(Context {
            connection: connection.clone(),
            processing_queue: Some(queue),
            backup_exports: crate::server::admin_backup::AdminBackupManager::default(),
            backup_imports: crate::server::admin_import::AdminImportManager::default(),
        });
    (
        TestServer::new(app).expect("test server should initialize"),
        connection,
    )
}

#[derive(Serialize)]
struct HtmlForm<'a> {
    title: &'a str,
    url: &'a str,
}

fn form_body(title: &str, url: &str) -> String {
    serde_urlencoded::to_string(HtmlForm { title, url }).expect("form should serialize")
}

fn assert_contains_all(body: &str, needles: &[&str]) {
    for needle in needles {
        assert!(body.contains(needle), "missing expected snippet: {needle}");
    }
}

fn seed_hyperlinks_insert_sql(count: usize) -> String {
    let mut sql = String::from(
        "INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at) VALUES ",
    );
    for id in 1..=count {
        if id > 1 {
            sql.push_str(", ");
        }
        sql.push_str(&format!(
                "({}, 'Link {}', 'https://example.com/{}', 'https://example.com/{}', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00')",
                id, id, id, id
            ));
    }
    sql.push(';');
    sql
}

async fn create_json_hyperlink(server: &TestServer, title: &str, url: &str) -> HyperlinkResponse {
    let create = server
        .post("/hyperlinks.json")
        .json(&json!({
            "title": title,
            "url": url,
        }))
        .await;
    create.assert_status(StatusCode::CREATED);
    create.json()
}

async fn show_json_hyperlink(server: &TestServer, id: i32) -> HyperlinkResponse {
    let show = server.get(&format!("/hyperlinks/{id}.json")).await;
    show.assert_status_ok();
    show.json()
}

async fn update_json_hyperlink(
    server: &TestServer,
    id: i32,
    title: &str,
    url: &str,
) -> HyperlinkResponse {
    let update = server
        .patch(&format!("/hyperlinks/{id}.json"))
        .json(&json!({
            "title": title,
            "url": url,
        }))
        .await;
    update.assert_status_ok();
    update.json()
}

async fn list_json_index(server: &TestServer, query: Option<&str>) -> HyperlinksIndexResponse {
    let path = match query {
        Some(query) => format!("/hyperlinks.json?{query}"),
        None => "/hyperlinks.json".to_string(),
    };
    let list = server.get(&path).await;
    list.assert_status_ok();
    list.json()
}

async fn list_json_hyperlinks(server: &TestServer) -> Vec<HyperlinkResponse> {
    list_json_index(server, None).await.items
}

async fn lookup_json(server: &TestServer, query: Option<&str>) -> HyperlinkLookupResponse {
    let path = match query {
        Some(query) => format!("/hyperlinks/lookup?{query}"),
        None => "/hyperlinks/lookup".to_string(),
    };
    let response = server.get(&path).await;
    response.assert_status_ok();
    response.json()
}

#[tokio::test]
async fn json_crud_flow_works() {
    let server = new_server().await;

    let created = create_json_hyperlink(&server, "Example", "https://example.com").await;
    assert_eq!(created.title, "Example");
    assert_eq!(created.raw_url, "https://example.com");
    assert_eq!(created.processing_state, "idle");

    let shown = show_json_hyperlink(&server, created.id).await;
    assert_eq!(shown.url, "https://example.com");
    assert_eq!(shown.raw_url, "https://example.com");

    let updated = update_json_hyperlink(
        &server,
        created.id,
        "Updated",
        "https://updated.example.com",
    )
    .await;
    assert_eq!(updated.title, "Updated");

    let delete = server.delete(&format!("/hyperlinks/{}", created.id)).await;
    delete.assert_status_see_other();

    server
        .get(&format!("/hyperlinks/{}.json", created.id))
        .await
        .assert_status_not_found();
}

#[tokio::test]
async fn json_create_autofills_empty_title() {
    let server = new_server().await;

    let created_model = create_json_hyperlink(&server, "", "https://example.com").await;
    assert_eq!(created_model.title, "https://example.com");
    assert_eq!(created_model.raw_url, "https://example.com");

    server
        .post("/hyperlinks.json")
        .json(&json!({
            "title": "Example",
            "url": "   ",
        }))
        .await
        .assert_status_bad_request();
}

#[tokio::test]
async fn html_create_invalid_input_rerenders_new_form_with_errors() {
    let server = new_server().await;

    let response = server
        .post("/hyperlinks")
        .text(form_body("", "mailto:test@example.com"))
        .content_type("application/x-www-form-urlencoded")
        .await;
    response.assert_status(StatusCode::UNPROCESSABLE_ENTITY);

    let body = response.text();
    assert!(body.contains("Please fix the highlighted issue."));
    assert!(body.contains("url must use http or https"));
    assert!(body.contains("value=\"mailto:test@example.com\""));
    assert!(body.contains("action=\"/hyperlinks\" method=\"post\""));
}

#[tokio::test]
async fn json_create_canonicalizes_query_params_and_preserves_raw_url() {
    let server = new_server().await;
    let created = create_json_hyperlink(
        &server,
        "Example",
        "https://example.com/docs?utm_source=newsletter&q=rust&fbclid=abc",
    )
    .await;

    assert_eq!(created.url, "https://example.com/docs?q=rust");
    assert_eq!(
        created.raw_url,
        "https://example.com/docs?utm_source=newsletter&q=rust&fbclid=abc"
    );
}

#[tokio::test]
async fn lookup_returns_invalid_url_without_query_param() {
    let server = new_server().await;
    let response = lookup_json(&server, None).await;
    assert_eq!(response.status, "invalid_url");
    assert!(response.id.is_none());
    assert!(response.canonical_url.is_none());
}

#[tokio::test]
async fn lookup_returns_invalid_url_for_unsupported_scheme() {
    let server = new_server().await;
    let response = lookup_json(&server, Some("url=mailto:test%40example.com")).await;
    assert_eq!(response.status, "invalid_url");
    assert!(response.id.is_none());
    assert!(response.canonical_url.is_none());
}

#[tokio::test]
async fn lookup_returns_not_found_for_valid_url() {
    let server = new_server().await;
    let response = lookup_json(
        &server,
        Some("url=https%3A%2F%2Fexample.com%2Fdocs%3Futm_source%3Dx%26q%3Drust"),
    )
    .await;
    assert_eq!(response.status, "not_found");
    assert!(response.id.is_none());
    assert_eq!(
        response.canonical_url.as_deref(),
        Some("https://example.com/docs?q=rust")
    );
}

#[tokio::test]
async fn lookup_returns_root_for_existing_root_link() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Root', 'https://example.com/root', 'https://example.com/root', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

    let response = lookup_json(&server, Some("url=https%3A%2F%2Fexample.com%2Froot")).await;
    assert_eq!(response.status, "root");
    assert_eq!(response.id, Some(1));
    assert_eq!(
        response.canonical_url.as_deref(),
        Some("https://example.com/root")
    );
}

#[tokio::test]
async fn lookup_returns_discovered_for_existing_discovered_link() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (7, 'Discovered', 'https://example.com/discovered', 'https://example.com/discovered', 1, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

    let response = lookup_json(&server, Some("url=https%3A%2F%2Fexample.com%2Fdiscovered")).await;
    assert_eq!(response.status, "discovered");
    assert_eq!(response.id, Some(7));
    assert_eq!(
        response.canonical_url.as_deref(),
        Some("https://example.com/discovered")
    );
}

#[tokio::test]
async fn visit_redirect_increments_click_count() {
    let server = new_server().await;

    let created = create_json_hyperlink(&server, "Example", "https://example.com").await;
    assert_eq!(created.clicks_count, 0);
    assert!(created.last_clicked_at.is_none());

    let visit = server
        .get(&format!("/hyperlinks/{}/visit", created.id))
        .await;
    visit.assert_status(StatusCode::TEMPORARY_REDIRECT);
    visit.assert_header("location", "https://example.com");

    let shown = show_json_hyperlink(&server, created.id).await;
    assert_eq!(shown.clicks_count, 1);
    assert!(shown.last_clicked_at.is_some());
}

#[tokio::test]
async fn click_endpoint_increments_click_count() {
    let server = new_server().await;

    let created = create_json_hyperlink(&server, "Example", "https://example.com").await;
    assert_eq!(created.clicks_count, 0);
    assert!(created.last_clicked_at.is_none());

    let click = server
        .post(&format!("/hyperlinks/{}/click", created.id))
        .await;
    click.assert_status(StatusCode::NO_CONTENT);

    let shown = show_json_hyperlink(&server, created.id).await;
    assert_eq!(shown.clicks_count, 1);
    assert!(shown.last_clicked_at.is_some());
}

#[tokio::test]
async fn html_pages_render() {
    let server = new_server().await;
    let created = create_json_hyperlink(&server, "Example", "https://example.com").await;

    let index = server.get("/hyperlinks").await;
    index.assert_status_ok();
    let index_body = index.text();
    assert_contains_all(
        &index_body,
        &[
            "<!DOCTYPE html>",
            "/hyperlinks/new",
            "href=\"https://example.com\"",
            "data-hyperlink-id=\"1\"",
        ],
    );
    assert!(
        index_body.contains("href=\"/hyperlinks/new\" class=\"inline-flex min-h-11 items-center")
    );
    assert!(index_body.contains("data-url-intent-input"));
    assert!(index_body.contains("data-url-intent"));
    assert!(index_body.contains("aria-hidden=\"true\""));
    assert!(index_body.contains("data-url-intent-add-button"));
    assert!(index_body.contains("data-url-intent-root-message"));
    assert!(index_body.contains("You know, you've already saved this link."));
    assert!(index_body.contains("id=\"hyperlinks-url-intent-add-form\""));
    assert!(index_body.contains("data-url-intent-add-form"));
    assert!(index_body.contains("data-url-intent-add-url"));
    assert!(index_body.contains("data-pdf-upload"));
    assert!(index_body.contains("data-pdf-upload-form"));
    assert!(index_body.contains("action=\"/uploads\""));
    assert!(index_body.contains("Choose PDFs"));
    assert!(index_body.contains("Drop PDFs to upload"));
    assert!(index_body.contains("multiple"));
    assert!(index_body.contains("Upload progress"));
    assert!(index_body.contains("data-pdf-upload-results"));
    assert!(index_body.contains("data-pdf-upload-result-template"));
    assert!(index_body.contains("View hyperlink"));
    assert!(index_body.contains("data-pdf-upload-overlay"));
    assert!(index_body.contains("motion-safe:animate-pulse"));
    assert!(index_body.contains("<details class=\"group sm:hidden\">"));
    assert!(index_body.contains("<summary"));
    assert!(index_body.contains("Filters"));
    assert!(index_body.contains("group-open:rotate-180"));
    assert!(index_body.contains(
        "class=\"hidden sm:flex sm:flex-row sm:flex-nowrap sm:items-center sm:gap-[0.4rem]\""
    ));
    assert!(index_body.contains("data-filter-key=\"status\""));
    assert!(index_body.contains("data-filter-key=\"type\""));
    assert!(index_body.contains("data-filter-key=\"order\""));
    assert!(index_body.contains("data-discovered-filter"));
    assert!(!index_body.contains("id=\"scope-filter\""));
    assert!(index_body.contains("class=\"grid grid-cols-1 gap-3 lg:grid-cols-2\""));
    assert!(
        index_body.contains("class=\"flex h-full flex-row items-start gap-2 min-w-0 sm:gap-4\"")
    );
    assert!(index_body.contains(">example.com</a>"));
    assert!(!index_body.contains(">https://example.com</a>"));
    assert!(index_body.contains(&format!("/hyperlinks/{}\">Details", created.id)));
    assert!(!index_body.contains("/hyperlinks/1/visit"));

    let new_page = server.get("/hyperlinks/new").await;
    new_page.assert_status_ok();
    let new_page_body = new_page.text();
    assert!(new_page_body.contains("Add Link or Upload PDF"));
    assert!(new_page_body.contains("action=\"/hyperlinks\" method=\"post\""));
    assert!(new_page_body.contains("data-pdf-upload"));
    assert!(new_page_body.contains("action=\"/uploads\""));

    let show = server.get(&format!("/hyperlinks/{}", created.id)).await;
    show.assert_status_ok();
    let show_body = show.text();
    assert_contains_all(
        &show_body,
        &["Artifacts", "Recent jobs", "Discovered links"],
    );
    assert!(show_body.contains(&format!("/hyperlinks/{}/delete", created.id)));

    let edit = server
        .get(&format!("/hyperlinks/{}/edit", created.id))
        .await;
    edit.assert_status_ok();
    assert!(
        edit.text()
            .contains(&format!("/hyperlinks/{}/update", created.id))
    );
}

#[tokio::test]
async fn index_card_shows_host_instead_of_full_url() {
    let server = new_server().await;
    create_json_hyperlink(
        &server,
        "Example",
        "https://www.example.com/articles/rust?x=1",
    )
    .await;

    let index = server.get("/hyperlinks").await;
    index.assert_status_ok();
    let body = index.text();

    assert!(body.contains("href=\"https://www.example.com/articles/rust?x=1\""));
    assert!(body.contains(">example.com</a>"));
    assert!(!body.contains(">www.example.com</a>"));
    assert!(!body.contains(">https://www.example.com/articles/rust?x=1</a>"));
}

#[tokio::test]
async fn show_missing_artifacts_uses_snapshot_source_for_non_pdf_links() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Article', 'https://example.com/article', 'https://example.com/article', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES (1, 1, NULL, 'readable_meta', X'7B7D', 'application/json', 2, '2026-02-19 00:00:01');
            "#,
        ))
        .await;

    let show = server.get("/hyperlinks/1").await;
    show.assert_status_ok();
    let body = show.text();
    assert!(body.contains("Missing:"));
    assert!(body.contains("Snapshot WARC"));
    assert!(body.contains("Readable Markdown"));
    assert!(!body.contains("PDF Source"));
}

#[tokio::test]
async fn show_missing_artifacts_uses_pdf_source_for_pdf_links() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, source_type, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Paper', 'https://example.com/paper.pdf', 'https://example.com/paper.pdf', 'pdf', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES (1, 1, NULL, 'readable_meta', X'7B7D', 'application/json', 2, '2026-02-19 00:00:01');
            "#,
        ))
        .await;

    let show = server.get("/hyperlinks/1").await;
    show.assert_status_ok();
    let body = show.text();
    assert!(body.contains("Missing:"));
    assert!(body.contains("PDF Source"));
    assert!(body.contains("Readable Markdown"));
    assert!(!body.contains("Snapshot WARC"));
    assert!(!body.contains("/hyperlinks/1/artifacts/screenshot_webp/fetch"));
    assert!(!body.contains("/hyperlinks/1/artifacts/screenshot_thumb_webp/fetch"));
    assert!(!body.contains("/hyperlinks/1/artifacts/screenshot_dark_webp/fetch"));
    assert!(!body.contains("/hyperlinks/1/artifacts/screenshot_thumb_dark_webp/fetch"));
}

#[tokio::test]
async fn show_missing_artifacts_requires_pdf_source_for_pdf_links_even_when_snapshot_exists() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, source_type, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Paper', 'https://example.com/paper.pdf', 'https://example.com/paper.pdf', 'pdf', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 1, NULL, 'snapshot_warc', X'57415243', 'application/warc', 4, '2026-02-19 00:00:01'),
                    (2, 1, NULL, 'readable_meta', X'7B7D', 'application/json', 2, '2026-02-19 00:00:02');
            "#,
        ))
        .await;

    let show = server.get("/hyperlinks/1").await;
    show.assert_status_ok();
    let body = show.text();
    assert!(body.contains("Missing:"));
    assert!(body.contains("/hyperlinks/1/artifacts/pdf_source/fetch"));
    assert!(!body.contains("/hyperlinks/1/artifacts/screenshot_webp/fetch"));
    assert!(!body.contains("/hyperlinks/1/artifacts/screenshot_thumb_webp/fetch"));
    assert!(body.contains("/hyperlinks/1/artifacts/readable_text/fetch"));
}

#[tokio::test]
async fn show_missing_artifacts_for_pdf_with_pdf_source_requires_only_thumbnails() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, source_type, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Paper', 'https://example.com/paper.pdf', 'https://example.com/paper.pdf', 'pdf', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 1, NULL, 'pdf_source', X'25504446', 'application/pdf', 4, '2026-02-19 00:00:01'),
                    (2, 1, NULL, 'screenshot_thumb_webp', X'52494646', 'image/webp', 4, '2026-02-19 00:00:02'),
                    (3, 1, NULL, 'readable_meta', X'7B7D', 'application/json', 2, '2026-02-19 00:00:03');
            "#,
        ))
        .await;

    let show = server.get("/hyperlinks/1").await;
    show.assert_status_ok();
    let body = show.text();
    assert!(!body.contains("/hyperlinks/1/artifacts/screenshot_webp/fetch"));
    assert!(!body.contains("/hyperlinks/1/artifacts/screenshot_dark_webp/fetch"));
    assert!(!body.contains("/hyperlinks/1/artifacts/screenshot_thumb_webp/fetch"));
    assert!(body.contains("/hyperlinks/1/artifacts/screenshot_thumb_dark_webp/fetch"));
}

#[tokio::test]
async fn show_missing_artifacts_prefers_existing_pdf_source_for_non_pdf_url() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Article', 'https://example.com/article', 'https://example.com/article', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 1, NULL, 'pdf_source', X'25504446', 'application/pdf', 4, '2026-02-19 00:00:01'),
                    (2, 1, NULL, 'readable_meta', X'7B7D', 'application/json', 2, '2026-02-19 00:00:02');
            "#,
        ))
        .await;

    let show = server.get("/hyperlinks/1").await;
    show.assert_status_ok();
    let body = show.text();
    assert!(body.contains("Missing:"));
    assert!(!body.contains("/hyperlinks/1/artifacts/snapshot_warc/fetch"));
    assert!(body.contains("/hyperlinks/1/artifacts/readable_text/fetch"));
}

#[tokio::test]
async fn show_artifacts_renders_delete_and_fetch_controls() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Article', 'https://example.com/article', 'https://example.com/article', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES (1, 1, NULL, 'readable_meta', X'7B7D', 'application/json', 2, '2026-02-19 00:00:01');
            "#,
        ))
        .await;

    let show = server.get("/hyperlinks/1").await;
    show.assert_status_ok();
    let body = show.text();
    assert!(body.contains("/hyperlinks/1/artifacts/readable_meta/delete"));
    assert!(body.contains("/hyperlinks/1/artifacts/snapshot_warc/fetch"));
    assert!(body.contains("/hyperlinks/1/artifacts/readable_text/fetch"));
}

#[tokio::test]
async fn show_ignores_oembed_title_when_no_open_graph_title() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'https://example.com/watch?v=1', 'https://example.com/watch?v=1', 'https://example.com/watch?v=1', 0, NULL, '2026-02-22 00:00:00', '2026-02-22 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES (
                    1,
                    1,
                    NULL,
                    'oembed_meta',
                    CAST('{"captured_at":"2026-02-22T00:00:30Z","selected":{"title":"Example Video Walkthrough","type":"video","provider_name":"VideoHost","author_name":"Codex Team","thumbnail_url":"https://img.example.com/thumb.jpg","url":"https://cdn.example.com/embed/1"}}' AS BLOB),
                    'application/json',
                    230,
                    '2026-02-22 00:00:31'
                );
            "#,
        ))
        .await;

    let show = server.get("/hyperlinks/1").await;
    show.assert_status_ok();
    let body = show.text();
    assert!(body.contains(">https://example.com/watch?v=1</h2>"));
    assert!(!body.contains("View full oEmbed JSON"));
    assert!(!body.contains(">oEmbed</h4>"));

    let shown = show_json_hyperlink(&server, 1).await;
    assert_eq!(shown.title, "https://example.com/watch?v=1");
}

#[tokio::test]
async fn show_uses_open_graph_title_when_current_title_is_url_like() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (
                    id,
                    title,
                    url,
                    raw_url,
                    og_title,
                    og_description,
                    og_type,
                    og_url,
                    og_image_url,
                    og_site_name,
                    clicks_count,
                    last_clicked_at,
                    created_at,
                    updated_at
                )
                VALUES (
                    1,
                    'https://example.com/watch?v=1',
                    'https://example.com/watch?v=1',
                    'https://example.com/watch?v=1',
                    'Example OG Video',
                    'OG description',
                    'video.other',
                    'https://example.com/watch?v=1',
                    'https://img.example.com/og.jpg',
                    'Example Site',
                    0,
                    NULL,
                    '2026-02-22 00:00:00',
                    '2026-02-22 00:00:00'
                );
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES (
                    1,
                    1,
                    NULL,
                    'og_meta',
                    CAST('{"captured_at":"2026-02-22T00:00:30Z","selected":{"title":"Example OG Video"}}' AS BLOB),
                    'application/json',
                    90,
                    '2026-02-22 00:00:31'
                );
            "#,
        ))
        .await;

    let show = server.get("/hyperlinks/1").await;
    show.assert_status_ok();
    let body = show.text();
    assert!(body.contains(">Example OG Video</h2>"));
    assert!(body.contains("Open Graph</h4>"));
}

#[tokio::test]
async fn show_decodes_html_entities_in_open_graph_fields() {
    let server = new_server_with_seed(Some(
        r#"
                INSERT INTO hyperlink (
                    id,
                    title,
                    url,
                    raw_url,
                    og_title,
                    og_description,
                    og_site_name,
                    clicks_count,
                    last_clicked_at,
                    created_at,
                    updated_at
                )
                VALUES (
                    1,
                    'https://example.com/article',
                    'https://example.com/article',
                    'https://example.com/article',
                    'Cats &amp; Dogs',
                    'Tips &amp; Tricks &#39;Daily&#39;',
                    'News &amp; Co',
                    0,
                    NULL,
                    '2026-02-22 00:00:00',
                    '2026-02-22 00:00:00'
                );
            "#,
    ))
    .await;

    let show = server.get("/hyperlinks/1").await;
    show.assert_status_ok();
    let body = show.text();
    assert!(body.contains(">Cats &amp; Dogs</h2>"));
    assert!(!body.contains(">Cats &amp;amp; Dogs</h2>"));
    assert!(body.contains("Tips &amp; Tricks"));
    assert!(body.contains("Daily"));
    assert!(!body.contains("Tips &amp;amp; Tricks"));
    assert!(!body.contains("&#38;#39;"));
    assert!(!body.contains("&amp;#39;"));
    assert!(body.contains("News &amp; Co"));
    assert!(!body.contains("News &amp;amp; Co"));
}

#[tokio::test]
async fn show_decodes_html_entities_in_fallback_title() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (
                    1,
                    'Cats &amp; Dogs',
                    'https://example.com/article',
                    'https://example.com/article',
                    0,
                    NULL,
                    '2026-02-22 00:00:00',
                    '2026-02-22 00:00:00'
                );
            "#,
        ))
        .await;

    let show = server.get("/hyperlinks/1").await;
    show.assert_status_ok();
    let body = show.text();
    assert!(body.contains(">Cats &amp; Dogs</h2>"));
    assert!(!body.contains(">Cats &amp;amp; Dogs</h2>"));
}

#[tokio::test]
async fn show_renders_dark_mode_aware_screenshot_when_artifacts_exist() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (
                    1,
                    'Example article',
                    'https://example.com/article',
                    'https://example.com/article',
                    0,
                    NULL,
                    '2026-02-22 00:00:00',
                    '2026-02-22 00:00:00'
                );
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 1, NULL, 'screenshot_webp', X'00', 'image/webp', 1, '2026-02-22 00:00:01'),
                    (2, 1, NULL, 'screenshot_dark_webp', X'00', 'image/webp', 1, '2026-02-22 00:00:02');
                INSERT INTO hyperlink_processing_job (id, hyperlink_id, kind, state, error_message, queued_at, started_at, finished_at, created_at, updated_at)
                VALUES
                    (42, 1, 'snapshot', 'succeeded', NULL, '2026-02-22 00:00:03', '2026-02-22 00:00:04', '2026-02-22 00:00:05', '2026-02-22 00:00:03', '2026-02-22 00:00:05');
            "#,
        ))
        .await;

    let show = server.get("/hyperlinks/1").await;
    show.assert_status_ok();
    let body = show.text();
    assert!(body.contains("/hyperlinks/1/artifacts/screenshot_webp/inline"));
    assert!(body.contains("/hyperlinks/1/artifacts/screenshot_dark_webp/inline"));
    assert!(body.contains("media=\"(prefers-color-scheme: dark)\""));
    assert!(body.contains("Screenshot for Example article"));
    assert!(body.contains("break-all text-sm"));
    assert!(body.contains("class=\"flex flex-row flex-wrap gap-4 text-sm\""));
    assert!(body.contains("class=\"flex flex-col gap-2 sm:flex-row sm:items-center sm:gap-4\""));
    assert!(body.contains("job#42"));
    assert!(body.matches("class=\"overflow-x-auto\"").count() >= 2);
}

#[tokio::test]
async fn show_renders_pdf_iframe_and_skips_screenshot_preview_for_pdf_sources() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, source_type, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (
                    1,
                    'Example paper',
                    'https://example.com/paper.pdf',
                    'https://example.com/paper.pdf',
                    'pdf',
                    0,
                    NULL,
                    '2026-02-22 00:00:00',
                    '2026-02-22 00:00:00'
                );
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 1, NULL, 'pdf_source', X'25504446', 'application/pdf', 4, '2026-02-22 00:00:01'),
                    (2, 1, NULL, 'screenshot_webp', X'00', 'image/webp', 1, '2026-02-22 00:00:02'),
                    (3, 1, NULL, 'screenshot_dark_webp', X'00', 'image/webp', 1, '2026-02-22 00:00:03');
            "#,
        ))
        .await;

    let show = server.get("/hyperlinks/1").await;
    show.assert_status_ok();
    let body = show.text();
    assert!(body.contains("<iframe"));
    assert!(body.contains("PDF preview for Example paper"));
    assert!(body.contains("/hyperlinks/1/artifacts/pdf_source/preview"));
    assert!(!body.contains("Screenshot for Example paper"));
}

#[tokio::test]
async fn index_decodes_html_entities_in_link_title_card() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (
                    1,
                    'Cats &amp; Dogs',
                    'https://example.com/article',
                    'https://example.com/article',
                    0,
                    NULL,
                    '2026-02-22 00:00:00',
                    '2026-02-22 00:00:00'
                );
            "#,
        ))
        .await;

    let index = server.get("/hyperlinks").await;
    index.assert_status_ok();
    let body = index.text();
    assert!(body.contains("Cats &amp; Dogs"));
    assert!(!body.contains("Cats &amp;amp; Dogs"));
}

#[tokio::test]
async fn show_decodes_html_entities_in_discovered_link_title_card() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Parent', 'https://example.com/parent', 'https://example.com/parent', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Cats &amp; Dogs', 'https://example.com/child', 'https://example.com/child', 1, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
                INSERT INTO hyperlink_relation (id, parent_hyperlink_id, child_hyperlink_id, created_at)
                VALUES (1, 1, 2, '2026-02-19 00:00:02');
            "#,
        ))
        .await;

    let show = server.get("/hyperlinks/1").await;
    show.assert_status_ok();
    let body = show.text();
    assert!(body.contains("Cats &amp; Dogs"));
    assert!(!body.contains("Cats &amp;amp; Dogs"));
}

#[tokio::test]
async fn show_prefers_open_graph_block_over_oembed_block_when_both_exist() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (
                    id,
                    title,
                    url,
                    raw_url,
                    og_title,
                    og_description,
                    og_type,
                    og_url,
                    og_image_url,
                    og_site_name,
                    clicks_count,
                    last_clicked_at,
                    created_at,
                    updated_at
                )
                VALUES (
                    1,
                    'https://example.com/watch?v=1',
                    'https://example.com/watch?v=1',
                    'https://example.com/watch?v=1',
                    'Example OG Video',
                    'OG description',
                    'video.other',
                    'https://example.com/watch?v=1',
                    'https://img.example.com/og.jpg',
                    'Example Site',
                    0,
                    NULL,
                    '2026-02-22 00:00:00',
                    '2026-02-22 00:00:00'
                );
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (
                        1,
                        1,
                        NULL,
                        'og_meta',
                        CAST('{"captured_at":"2026-02-22T00:00:30Z","selected":{"title":"Example OG Video","description":"OG description"}}' AS BLOB),
                        'application/json',
                        120,
                        '2026-02-22 00:00:31'
                    ),
                    (
                        2,
                        1,
                        NULL,
                        'oembed_meta',
                        CAST('{"captured_at":"2026-02-22T00:00:30Z","selected":{"title":"Example oEmbed Video","type":"video","provider_name":"VideoHost"}}' AS BLOB),
                        'application/json',
                        140,
                        '2026-02-22 00:00:31'
                    );
            "#,
        ))
        .await;

    let show = server.get("/hyperlinks/1").await;
    show.assert_status_ok();
    let body = show.text();
    assert!(body.contains("Open Graph</h4>"));
    assert!(body.contains("View full Open Graph JSON"));
    assert!(!body.contains(">oEmbed</h4>"));
    assert!(!body.contains("View full oEmbed JSON"));
}

#[tokio::test]
async fn index_failed_status_shows_failed_badge() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example', 'https://example.com', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_processing_job (id, hyperlink_id, kind, state, error_message, queued_at, started_at, finished_at, created_at, updated_at)
                VALUES (42, 1, 'snapshot', 'failed', 'snapshot request failed', '2026-02-19 00:01:00', '2026-02-19 00:01:10', '2026-02-19 00:01:20', '2026-02-19 00:01:00', '2026-02-19 00:01:20');
            "#,
        ))
        .await;

    let index = server.get("/hyperlinks").await;
    index.assert_status_ok();
    let body = index.text();
    assert_contains_all(&body, &["Failed"]);
    assert!(!body.contains("/jobs/42"));
}

#[tokio::test]
async fn artifact_download_endpoint_uses_latest_artifact_per_kind() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example', 'https://example.com', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 1, NULL, 'readable_text', X'6669727374', 'text/markdown; charset=utf-8', 5, '2026-02-19 00:00:01'),
                    (2, 1, NULL, 'readable_text', X'7365636f6e642070726576696577', 'text/markdown; charset=utf-8', 14, '2026-02-19 00:00:02'),
                    (3, 1, NULL, 'pdf_source', X'255044462D312E34', 'application/pdf', 8, '2026-02-19 00:00:01'),
                    (4, 1, NULL, 'pdf_source', X'255044462D312E350A25', 'application/pdf', 10, '2026-02-19 00:00:03'),
                    (5, 1, NULL, 'snapshot_warc', X'57415243', 'application/warc', 4, '2026-02-19 00:00:04'),
                    (6, 1, NULL, 'snapshot_warc', X'1F8B0800920EA06900030B770C7256284ECC2DC8490500D757B83F0B000000', 'application/warc+gzip', 31, '2026-02-19 00:00:05');
            "#,
        ))
        .await;

    let download = server.get("/hyperlinks/1/artifacts/readable_text").await;
    download.assert_status_ok();
    download.assert_header("content-type", "text/markdown; charset=utf-8");
    download.assert_header(
        "content-disposition",
        "attachment; filename=\"hyperlink-1-readable_text.md\"",
    );
    assert_eq!(download.text(), "second preview");

    let pdf_download = server.get("/hyperlinks/1/artifacts/pdf_source").await;
    pdf_download.assert_status_ok();
    pdf_download.assert_header("content-type", "application/pdf");
    pdf_download.assert_header(
        "content-disposition",
        "attachment; filename=\"hyperlink-1-pdf_source.pdf\"",
    );
    assert_eq!(pdf_download.text(), "%PDF-1.5\n%");

    let inline = server
        .get("/hyperlinks/1/artifacts/readable_text/inline")
        .await;
    inline.assert_status_ok();
    inline.assert_header("content-type", "text/markdown; charset=utf-8");
    assert_eq!(inline.text(), "second preview");

    let pdf_inline = server
        .get("/hyperlinks/1/artifacts/pdf_source/inline")
        .await;
    pdf_inline.assert_status_ok();
    pdf_inline.assert_header("content-type", "application/pdf");
    assert_eq!(pdf_inline.text(), "%PDF-1.5\n%");

    let pdf_preview = server
        .get("/hyperlinks/1/artifacts/pdf_source/preview")
        .await;
    pdf_preview.assert_status_ok();
    pdf_preview.assert_header("content-type", "text/html; charset=utf-8");
    let pdf_preview_body = pdf_preview.text();
    assert!(pdf_preview_body.contains("color-scheme: light;"));
    assert!(
        pdf_preview_body
            .contains("<embed src=\"/hyperlinks/1/artifacts/pdf_source/inline#zoom=page-width\"")
    );

    let warc_download = server.get("/hyperlinks/1/artifacts/snapshot_warc").await;
    warc_download.assert_status_ok();
    warc_download.assert_header("content-type", "application/warc+gzip");
    warc_download.assert_header(
        "content-disposition",
        "attachment; filename=\"hyperlink-1-snapshot_warc.warc.gz\"",
    );
    assert!(warc_download.as_bytes().starts_with(&[0x1f, 0x8b]));

    let warc_inline = server
        .get("/hyperlinks/1/artifacts/snapshot_warc/inline")
        .await;
    warc_inline.assert_status_ok();
    warc_inline.assert_header("content-type", "application/warc+gzip");
    assert!(warc_inline.as_bytes().starts_with(&[0x1f, 0x8b]));

    server
        .get("/hyperlinks/1/artifacts/not_a_kind")
        .await
        .assert_status_bad_request();
    server
        .get("/hyperlinks/999/artifacts/readable_text")
        .await
        .assert_status_not_found();
}

#[tokio::test]
async fn artifact_download_endpoint_keeps_legacy_snapshot_warc_extension_for_plain_content_type() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example', 'https://example.com', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 1, NULL, 'snapshot_warc', X'57415243', 'application/warc', 4, '2026-02-19 00:00:01');
            "#,
        ))
        .await;

    let download = server.get("/hyperlinks/1/artifacts/snapshot_warc").await;
    download.assert_status_ok();
    download.assert_header("content-type", "application/warc");
    download.assert_header(
        "content-disposition",
        "attachment; filename=\"hyperlink-1-snapshot_warc.warc\"",
    );
    assert_eq!(download.text(), "WARC");
}

#[tokio::test]
async fn delete_artifact_kind_removes_all_rows_for_that_kind() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example', 'https://example.com', 'https://example.com', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 1, NULL, 'readable_text', X'6669727374', 'text/markdown; charset=utf-8', 5, '2026-02-19 00:00:01'),
                    (2, 1, NULL, 'readable_text', X'7365636f6e64', 'text/markdown; charset=utf-8', 6, '2026-02-19 00:00:02');
            "#,
        ))
        .await;

    let delete = server
        .post("/hyperlinks/1/artifacts/readable_text/delete")
        .await;
    delete.assert_status_see_other();
    delete.assert_header("location", "/hyperlinks/1");

    server
        .get("/hyperlinks/1/artifacts/readable_text")
        .await
        .assert_status_not_found();
}

#[tokio::test]
async fn delete_readability_artifact_clears_search_doc_text() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example', 'https://example.com', 'https://example.com', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES (1, 1, NULL, 'readable_text', CAST('quantumneedle appears here' AS BLOB), 'text/markdown; charset=utf-8', 24, '2026-02-19 00:00:01');
            "#,
        ))
        .await;

    let before = server.get("/hyperlinks?q=quantumneedle").await;
    before.assert_status_ok();
    assert!(before.text().contains("/hyperlinks/1\">Details"));

    let delete = server
        .post("/hyperlinks/1/artifacts/readable_text/delete")
        .await;
    delete.assert_status_see_other();

    let after = server.get("/hyperlinks?q=quantumneedle").await;
    after.assert_status_ok();
    assert!(!after.text().contains("/hyperlinks/1\">Details"));
}

#[tokio::test]
async fn fetch_artifact_kind_enqueues_snapshot_job() {
    let (server, connection) = new_server_with_queue(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example', 'https://example.com', 'https://example.com', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES (1, 1, NULL, 'snapshot_warc', X'77617263', 'application/warc', 4, '2026-02-19 00:00:01');
            "#,
        ))
        .await;

    let fetch = server
        .post("/hyperlinks/1/artifacts/screenshot_webp/fetch")
        .await;
    fetch.assert_status_see_other();
    fetch.assert_header("location", "/hyperlinks/1");

    let latest = crate::app::models::hyperlink_processing_job::latest_for_hyperlink(&connection, 1)
        .await
        .expect("latest job should load")
        .expect("job should exist");
    assert_eq!(
        latest.kind,
        hyperlink_processing_job::HyperlinkProcessingJobKind::Snapshot
    );
}

#[tokio::test]
async fn fetch_artifact_kind_blocks_pdf_thumbnail_fetch_until_pdf_source_exists() {
    let (server, connection) = new_server_with_queue(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, source_type, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Uploaded PDF', '/uploads/1/paper.pdf', '/uploads/1/paper.pdf', 'pdf', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

    let fetch = server
        .post("/hyperlinks/1/artifacts/screenshot_thumb_webp/fetch")
        .await;
    fetch.assert_status_see_other();
    fetch.assert_header("location", "/hyperlinks/1");

    let latest = crate::app::models::hyperlink_processing_job::latest_for_hyperlink(&connection, 1)
        .await
        .expect("latest job query should succeed");
    assert!(latest.is_none());
}

#[tokio::test]
async fn fetch_artifact_kind_enqueues_processing_jobs_for_og_and_readability_requests() {
    let (server, connection) = new_server_with_queue(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example', 'https://example.com', 'https://example.com', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

    let og_fetch = server.post("/hyperlinks/1/artifacts/og_meta/fetch").await;
    og_fetch.assert_status_see_other();

    let og_latest =
        crate::app::models::hyperlink_processing_job::latest_for_hyperlink(&connection, 1)
            .await
            .expect("latest job should load")
            .expect("job should exist");
    assert!(
        matches!(
            og_latest.kind,
            hyperlink_processing_job::HyperlinkProcessingJobKind::Snapshot
                | hyperlink_processing_job::HyperlinkProcessingJobKind::Og
        ),
        "expected og fetch to enqueue either the requested job or its source dependency"
    );

    let readable_fetch = server
        .post("/hyperlinks/1/artifacts/readable_text/fetch")
        .await;
    readable_fetch.assert_status_see_other();

    let readable_latest =
        crate::app::models::hyperlink_processing_job::latest_for_hyperlink(&connection, 1)
            .await
            .expect("latest job should load")
            .expect("job should exist");
    assert!(
        matches!(
            readable_latest.kind,
            hyperlink_processing_job::HyperlinkProcessingJobKind::Snapshot
                | hyperlink_processing_job::HyperlinkProcessingJobKind::Readability
        ),
        "expected readability fetch to enqueue either the requested job or its source dependency"
    );
}

#[tokio::test]
async fn fetch_artifact_kind_without_queue_keeps_processing_idle() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example', 'https://example.com', 'https://example.com', 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

    let fetch = server
        .post("/hyperlinks/1/artifacts/readable_text/fetch")
        .await;
    fetch.assert_status_see_other();
    fetch.assert_header("location", "/hyperlinks/1");

    let shown = show_json_hyperlink(&server, 1).await;
    assert_eq!(shown.processing_state, "idle");
}

#[tokio::test]
async fn html_write_flows_redirect() {
    let server = new_server().await;

    let create = server
        .post("/hyperlinks")
        .text(form_body("Example", "https://example.com"))
        .content_type("application/x-www-form-urlencoded")
        .await;
    create.assert_status_see_other();
    create.assert_header("location", "/hyperlinks/1");

    let reprocess = server.post("/hyperlinks/1/reprocess").await;
    reprocess.assert_status_see_other();
    reprocess.assert_header("location", "/hyperlinks/1");

    let update = server
        .post("/hyperlinks/1/update")
        .text(form_body("Updated", "https://updated.example.com"))
        .content_type("application/x-www-form-urlencoded")
        .await;
    update.assert_status_see_other();
    update.assert_header("location", "/hyperlinks/1");

    let delete = server.post("/hyperlinks/1/delete").await;
    delete.assert_status_see_other();
    delete.assert_header("location", "/hyperlinks");

    server
        .get("/hyperlinks/1.json")
        .await
        .assert_status_not_found();
}

#[tokio::test]
async fn html_update_invalid_input_rerenders_edit_form_with_errors() {
    let server = new_server().await;
    let created = create_json_hyperlink(&server, "Example", "https://example.com").await;

    let update = server
        .post(&format!("/hyperlinks/{}/update", created.id))
        .text(form_body("Broken", "mailto:test@example.com"))
        .content_type("application/x-www-form-urlencoded")
        .await;
    update.assert_status(StatusCode::UNPROCESSABLE_ENTITY);

    let body = update.text();
    assert!(body.contains("Please fix the highlighted issue."));
    assert!(body.contains("url must use http or https"));
    assert!(body.contains("value=\"Broken\""));
    assert!(body.contains("value=\"mailto:test@example.com\""));
    assert!(body.contains(&format!("action=\"/hyperlinks/{}/update\"", created.id)));
}

#[tokio::test]
async fn path_id_params_take_precedence_over_query_id_params() {
    let server = new_server().await;
    let first = create_json_hyperlink(&server, "First", "https://example.com/first").await;
    let second = create_json_hyperlink(&server, "Second", "https://example.com/second").await;

    let show = server
        .get(&format!("/hyperlinks/{}.json?id={}", first.id, second.id))
        .await;
    show.assert_status_ok();
    let shown: HyperlinkResponse = show.json();

    assert_eq!(shown.id, first.id);
    assert_eq!(shown.title, "First");
}

#[tokio::test]
async fn malformed_json_body_returns_json_bad_request() {
    let server = new_server().await;

    let response = server
        .post("/hyperlinks.json")
        .text("{not valid json")
        .content_type("application/json")
        .await;
    response.assert_status_bad_request();

    let body = response.text();
    assert!(body.contains("\"error\""));
    assert!(body.contains("failed to parse json body"));
}

#[tokio::test]
async fn index_hides_discovered_links_and_show_displays_them() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Parent', 'https://example.com/parent', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Child', 'https://example.com/child', 1, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
                INSERT INTO hyperlink_relation (id, parent_hyperlink_id, child_hyperlink_id, created_at)
                VALUES (1, 1, 2, '2026-02-19 00:00:02');
            "#,
        ))
        .await;

    let index = server.get("/hyperlinks").await;
    index.assert_status_ok();
    let index_body = index.text();
    assert!(index_body.contains("Parent"));
    assert!(!index_body.contains("Child"));

    let listed = list_json_hyperlinks(&server).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].title, "Parent");

    let show = server.get("/hyperlinks/1").await;
    show.assert_status_ok();
    let show_body = show.text();
    assert_contains_all(&show_body, &["Discovered links", "Child", "/hyperlinks/2"]);
}

#[tokio::test]
async fn direct_add_promotes_discovered_link_to_root() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Discovered', 'https://example.com/child', 1, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

    let promoted =
        create_json_hyperlink(&server, "Added Directly", "https://example.com/child").await;
    assert_eq!(promoted.id, 1);
    assert_eq!(promoted.title, "Added Directly");

    let listed = list_json_hyperlinks(&server).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, 1);
    assert_eq!(listed[0].title, "Added Directly");
}

#[tokio::test]
async fn index_query_scope_all_includes_discovered_links_in_html_and_json() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Root', 'https://example.com/root', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Discovered', 'https://example.com/discovered', 1, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
            "#,
        ))
        .await;

    let html = server.get("/hyperlinks?q=scope:all").await;
    html.assert_status_ok();
    let html_body = html.text();
    assert!(html_body.contains("Root"));
    assert!(html_body.contains("Discovered"));

    let json = list_json_index(&server, Some("q=scope:all")).await;
    let titles = json
        .items
        .iter()
        .map(|item| item.title.as_str())
        .collect::<Vec<_>>();
    assert_eq!(titles.len(), 2);
    assert!(titles.contains(&"Root"));
    assert!(titles.contains(&"Discovered"));
}

#[tokio::test]
async fn index_query_with_discovered_includes_discovered_links_in_html_and_json() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Root', 'https://example.com/root', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Discovered', 'https://example.com/discovered', 1, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
            "#,
        ))
        .await;

    let html = server.get("/hyperlinks?q=with:discovered").await;
    html.assert_status_ok();
    let html_body = html.text();
    assert!(html_body.contains("Root"));
    assert!(html_body.contains("Discovered"));
    assert!(html_body.contains("<details class=\"group sm:hidden\" open>"));
    assert!(html_body.contains("data-discovered-filter"));
    assert!(html_body.contains("checked"));

    let json = list_json_index(&server, Some("q=with:discovered")).await;
    let titles = json
        .items
        .iter()
        .map(|item| item.title.as_str())
        .collect::<Vec<_>>();
    assert_eq!(titles.len(), 2);
    assert!(titles.contains(&"Root"));
    assert!(titles.contains(&"Discovered"));
}

#[tokio::test]
async fn index_query_status_failed_filters_by_latest_processing_job_state() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Failed', 'https://example.com/failed', 'https://example.com/failed', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Processing', 'https://example.com/processing', 'https://example.com/processing', 0, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01'),
                    (3, 'Idle', 'https://example.com/idle', 'https://example.com/idle', 0, 0, NULL, '2026-02-19 00:00:02', '2026-02-19 00:00:02');
                INSERT INTO hyperlink_processing_job (id, hyperlink_id, kind, state, error_message, queued_at, started_at, finished_at, created_at, updated_at)
                VALUES
                    (10, 1, 'snapshot', 'failed', 'failed', '2026-02-19 00:01:00', '2026-02-19 00:01:10', '2026-02-19 00:01:20', '2026-02-19 00:01:00', '2026-02-19 00:01:20'),
                    (11, 2, 'snapshot', 'running', NULL, '2026-02-19 00:02:00', '2026-02-19 00:02:10', NULL, '2026-02-19 00:02:00', '2026-02-19 00:02:10');
            "#,
        ))
        .await;

    let json = list_json_index(&server, Some("q=status:failed")).await;
    assert_eq!(json.items.len(), 1);
    assert_eq!(json.items[0].title, "Failed");

    let html = server.get("/hyperlinks?q=status:failed").await;
    html.assert_status_ok();
    let body = html.text();
    assert!(body.contains("Failed"));
    assert!(!body.contains("https://example.com/processing"));
    assert!(!body.contains("https://example.com/idle"));
}

#[tokio::test]
async fn index_query_returns_diagnostics_and_random_order_selection() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'One', 'https://example.com/1', 'https://example.com/1', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Two', 'https://example.com/2', 'https://example.com/2', 0, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
            "#,
        ))
        .await;

    let response = list_json_index(&server, Some("q=order:random+status:not-real")).await;
    assert_eq!(response.items.len(), 2);
    assert_eq!(response.query.raw_q, "order:random status:not-real");
    assert_eq!(response.query.parsed.orders.len(), 1);
    assert_eq!(
        response.query.parsed.orders[0],
        crate::server::hyperlink_fetcher::OrderToken::Random
    );
    assert_eq!(
        response.query.ignored_tokens,
        vec!["status:not-real".to_string()]
    );
    assert!(response.query.free_text.is_empty());
}

#[tokio::test]
async fn index_hides_relevance_sort_option_without_free_text() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Article', 'https://example.com/article', 'https://example.com/article', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

    let html = server.get("/hyperlinks").await;
    html.assert_status_ok();
    let body = html.text();
    assert!(!body.contains("value=\"relevance\""));
}

#[tokio::test]
async fn index_shows_relevance_sort_option_with_free_text() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Rust article', 'https://example.com/rust', 'https://example.com/rust', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

    let html = server.get("/hyperlinks?q=rust").await;
    html.assert_status_ok();
    let body = html.text();
    assert!(body.contains("value=\"relevance\""));
}

#[tokio::test]
async fn index_shows_newest_sort_option_as_default() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Article', 'https://example.com/article', 'https://example.com/article', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

    let html = server.get("/hyperlinks").await;
    html.assert_status_ok();
    let body = html.text();
    assert!(!body.contains("<option value=\"\">Status</option>"));
    assert!(!body.contains("<option value=\"\">Type</option>"));
    assert!(!body.contains("<option value=\"\">Sort</option>"));
    assert!(!body.contains("id=\"scope-filter\""));
    assert!(body.contains("data-discovered-filter"));
    assert!(body.contains("value=\"all\" selected"));
    assert!(body.contains("value=\"newest\""));
    assert!(body.contains("value=\"newest\" selected"));
}

#[tokio::test]
async fn index_shows_no_matches_copy_when_filters_exclude_all_links() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Article', 'https://example.com/article', 'https://example.com/article', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

    let html = server.get("/hyperlinks?q=type:pdf").await;
    html.assert_status_ok();
    let body = html.text();
    assert!(body.contains("No hyperlinks match the current filters."));
    assert!(!body.contains("No hyperlinks yet."));
}

#[tokio::test]
async fn index_query_type_pdf_uses_source_type_not_url_or_artifact_fallbacks() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, source_type, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'ArXiv', 'https://arxiv.org/pdf/2602.11988', 'https://arxiv.org/pdf/2602.11988', 'pdf', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Pdf Suffix Html', 'https://example.com/report.pdf', 'https://example.com/report.pdf', 'html', 0, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01'),
                    (3, 'Pending Pdf', 'https://example.com/pending.pdf', 'https://example.com/pending.pdf', 'pdf', 0, 0, NULL, '2026-02-19 00:00:02', '2026-02-19 00:00:02'),
                    (4, 'Article', 'https://example.com/article', 'https://example.com/article', 'html', 0, 0, NULL, '2026-02-19 00:00:03', '2026-02-19 00:00:03');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (10, 1, NULL, 'snapshot_warc', X'57415243', 'application/warc', 4, '2026-02-19 00:00:04'),
                    (11, 2, NULL, 'pdf_source', X'25504446', 'application/pdf', 4, '2026-02-19 00:00:05');
            "#,
        ))
        .await;

    let pdf = list_json_index(&server, Some("q=type:pdf")).await;
    let pdf_titles = pdf
        .items
        .iter()
        .map(|item| item.title.as_str())
        .collect::<Vec<_>>();
    assert_eq!(pdf_titles.len(), 2, "pdf titles: {:?}", pdf_titles);
    assert!(pdf_titles.contains(&"ArXiv"));
    assert!(pdf_titles.contains(&"Pending Pdf"));
    assert!(!pdf_titles.contains(&"Pdf Suffix Html"));

    let non_pdf = list_json_index(&server, Some("q=type:non-pdf")).await;
    let non_pdf_titles = non_pdf
        .items
        .iter()
        .map(|item| item.title.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        non_pdf_titles.len(),
        2,
        "non-pdf titles: {:?}",
        non_pdf_titles
    );
    assert!(non_pdf_titles.contains(&"Pdf Suffix Html"));
    assert!(non_pdf_titles.contains(&"Article"));
    assert!(!non_pdf_titles.contains(&"ArXiv"));
}

#[tokio::test]
async fn index_renders_pdf_badge_for_pdf_source_type_without_pdf_suffix() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, source_type, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'ArXiv', 'https://arxiv.org/pdf/2602.11988', 'https://arxiv.org/pdf/2602.11988', 'pdf', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        ))
        .await;

    let html = server.get("/hyperlinks").await;
    html.assert_status_ok();
    let body = html.text();
    assert!(body.contains("ArXiv"));
    assert!(body.contains("PDF</span>"));
}

#[tokio::test]
async fn index_renders_rss_badge_for_feed_imported_links() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO rss_feed (id, url, title, site_url, active, poll_interval_secs, last_fetched_at, created_at, updated_at)
                VALUES
                    (1, 'https://example.com/feed.xml', 'Example Feed', 'https://example.com', 1, 1800, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
                INSERT INTO hyperlink (id, title, url, raw_url, source_type, rss_feed_id, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Feed Post', 'https://example.com/posts/1', 'https://example.com/posts/1', 'html', 1, 0, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
            "#,
        ))
        .await;

    let html = server.get("/hyperlinks").await;
    html.assert_status_ok();
    let body = html.text();
    assert!(body.contains("Feed Post"));
    assert!(body.contains("RSS</span>"));
}

#[tokio::test]
async fn index_uses_dark_thumbnail_artifacts_without_frontend_pdf_filter() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, source_type, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'PDF Link', 'https://arxiv.org/pdf/2602.11988', 'https://arxiv.org/pdf/2602.11988', 'pdf', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'HTML Link', 'https://example.com/article', 'https://example.com/article', 'html', 0, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (10, 1, NULL, 'pdf_source', X'25504446', 'application/pdf', 4, '2026-02-19 00:00:02'),
                    (11, 1, NULL, 'screenshot_thumb_webp', X'52494646', 'image/webp', 4, '2026-02-19 00:00:03'),
                    (12, 1, NULL, 'screenshot_thumb_dark_webp', X'52494646', 'image/webp', 4, '2026-02-19 00:00:04'),
                    (13, 2, NULL, 'screenshot_thumb_webp', X'52494646', 'image/webp', 4, '2026-02-19 00:00:05');
            "#,
        ))
        .await;

    let html = server.get("/hyperlinks").await;
    html.assert_status_ok();
    let body = html.text();

    assert!(body.contains("/hyperlinks/1/artifacts/screenshot_thumb_dark_webp/inline"));
    assert!(body.contains("media=\"(prefers-color-scheme: dark)\""));
    assert!(!body.contains("pdf-thumbnail-neutral-invert"));
    assert!(!body.contains("id=\"pdf-neutral-invert\""));
}

#[tokio::test]
async fn index_query_free_text_matches_readable_text_content() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Alpha', 'https://example.com/a', 'https://example.com/a', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Beta', 'https://example.com/b', 'https://example.com/b', 0, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (10, 1, NULL, 'readable_text', CAST('rust systems guide' AS BLOB), 'text/markdown; charset=utf-8', 18, '2026-02-19 00:00:02'),
                    (11, 2, NULL, 'readable_text', CAST('python scripting notes' AS BLOB), 'text/markdown; charset=utf-8', 22, '2026-02-19 00:00:03');
            "#,
        ))
        .await;

    let response = list_json_index(&server, Some("q=rust")).await;
    assert_eq!(response.items.len(), 1);
    assert_eq!(response.items[0].title, "Alpha");
}

#[tokio::test]
async fn index_query_free_text_renders_match_snippet_in_html() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Alpha', 'https://example.com/a', 'https://example.com/a', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Beta', 'https://example.com/rust-link', 'https://example.com/rust-link', 0, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (10, 1, NULL, 'readable_text', CAST('this readable text mentions rust and systems' AS BLOB), 'text/markdown; charset=utf-8', 44, '2026-02-19 00:00:02');
            "#,
        ))
        .await;

    let html = server.get("/hyperlinks?q=rust").await;
    html.assert_status_ok();
    let body = html.text();
    assert!(body.contains("this readable text mentions <em>rust</em> and systems"));
    assert!(body.contains("https://example.com/<em>rust</em>-link"));
}

#[tokio::test]
async fn index_query_quoted_term_matches_exact_word_only() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Parsers guide', 'https://example.com/parsers', 'https://example.com/parsers', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Parser guide', 'https://example.com/parser', 'https://example.com/parser', 0, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
            "#,
        ))
        .await;

    let response = list_json_index(&server, Some("q=%22parser%22")).await;
    assert_eq!(response.items.len(), 1);
    assert_eq!(response.items[0].title, "Parser guide");

    let html = server.get("/hyperlinks?q=%22parser%22").await;
    html.assert_status_ok();
    let body = html.text();
    assert!(body.contains("Parser guide"));
    assert!(!body.contains("Parsers guide"));
}

#[tokio::test]
async fn index_query_free_text_falls_back_to_title_url_for_missing_readability() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Rust no readability', 'https://example.com/rust-no-readability', 'https://example.com/rust-no-readability', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Rust with readability mismatch', 'https://example.com/rust-with-readability', 'https://example.com/rust-with-readability', 0, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (10, 2, NULL, 'readable_text', CAST('python only body' AS BLOB), 'text/markdown; charset=utf-8', 16, '2026-02-19 00:00:02');
            "#,
        ))
        .await;

    let response = list_json_index(&server, Some("q=rust")).await;
    assert_eq!(response.items.len(), 2);
    let ids = response
        .items
        .iter()
        .map(|item| item.id)
        .collect::<Vec<_>>();
    assert!(ids.contains(&1));
    assert!(ids.contains(&2));
}

#[tokio::test]
async fn index_query_order_relevance_without_text_falls_back_to_newest() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Older', 'https://example.com/older', 'https://example.com/older', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Newer', 'https://example.com/newer', 'https://example.com/newer', 0, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
            "#,
        ))
        .await;

    let response = list_json_index(&server, Some("q=order:relevance")).await;
    assert_eq!(response.items.len(), 2);
    assert_eq!(response.items[0].title, "Newer");
    assert_eq!(response.items[1].title, "Older");
}

#[tokio::test]
async fn index_query_explicit_order_overrides_default_relevance_ordering() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Older', 'https://example.com/older', 'https://example.com/older', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Newer', 'https://example.com/newer', 'https://example.com/newer', 0, 0, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (10, 1, NULL, 'readable_text', CAST('rust article' AS BLOB), 'text/markdown; charset=utf-8', 12, '2026-02-19 00:00:02'),
                    (11, 2, NULL, 'readable_text', CAST('rust article' AS BLOB), 'text/markdown; charset=utf-8', 12, '2026-02-19 00:00:03');
            "#,
        ))
        .await;

    let response = list_json_index(&server, Some("q=rust+order:oldest")).await;
    assert_eq!(response.items.len(), 2);
    assert_eq!(response.items[0].title, "Older");
    assert_eq!(response.items[1].title, "Newer");
}

#[tokio::test]
async fn index_json_paginates_100_per_page() {
    let seed_sql = seed_hyperlinks_insert_sql(205);
    let server = new_server_with_seed(Some(seed_sql.as_str())).await;

    let page_1 = list_json_index(&server, None).await;
    assert_eq!(page_1.items.len(), 100);
    assert_eq!(page_1.items[0].id, 205);
    assert_eq!(page_1.items[99].id, 106);

    let page_2 = list_json_index(&server, Some("page=2")).await;
    assert_eq!(page_2.items.len(), 100);
    assert_eq!(page_2.items[0].id, 105);
    assert_eq!(page_2.items[99].id, 6);

    let page_3 = list_json_index(&server, Some("page=3")).await;
    assert_eq!(page_3.items.len(), 5);
    assert_eq!(page_3.items[0].id, 5);
    assert_eq!(page_3.items[4].id, 1);

    let clamped = list_json_index(&server, Some("page=99")).await;
    assert_eq!(clamped.items.len(), 5);
    assert_eq!(clamped.items[0].id, 5);
    assert_eq!(clamped.items[4].id, 1);
}

#[tokio::test]
async fn index_html_renders_pagination_links_and_preserves_query() {
    let seed_sql = seed_hyperlinks_insert_sql(101);
    let server = new_server_with_seed(Some(seed_sql.as_str())).await;

    let first_page = server.get("/hyperlinks?q=link").await;
    first_page.assert_status_ok();
    let first_body = first_page.text();
    assert!(first_body.contains("Page 1 of 2"));
    assert!(first_body.contains("/hyperlinks?q=link&amp;page=2"));
    assert!(first_body.contains("Details"));

    let second_page = server.get("/hyperlinks?q=link&page=2").await;
    second_page.assert_status_ok();
    let second_body = second_page.text();
    assert!(second_body.contains("Page 2 of 2"));
    assert!(second_body.contains("/hyperlinks?q=link&amp;page=1"));
    assert!(second_body.contains("/hyperlinks/1\">Details"));
    assert!(!second_body.contains("/hyperlinks/101\">Details"));

    let clamped_page = server.get("/hyperlinks?q=link&page=99").await;
    clamped_page.assert_status_ok();
    let clamped_body = clamped_page.text();
    assert!(clamped_body.contains("Page 2 of 2"));
    assert!(clamped_body.contains("/hyperlinks?q=link&amp;page=1"));
}

#[tokio::test]
async fn index_json_cleans_site_suffix_titles() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Understanding Rust Lifetimes | Example.com', 'https://example.com/rust/lifetimes', 'https://example.com/rust/lifetimes', 0, 0, NULL, '2026-02-25 00:00:00', '2026-02-25 00:00:00');
            "#,
        ))
        .await;

    let response = list_json_index(&server, None).await;
    assert_eq!(response.items.len(), 1);
    assert_eq!(response.items[0].title, "Understanding Rust Lifetimes");
}

#[tokio::test]
async fn index_json_preserves_non_site_dash_titles() {
    let server = new_server_with_seed(Some(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Rust - The Book', 'https://doc.rust-lang.org/book', 'https://doc.rust-lang.org/book', 0, 0, NULL, '2026-02-25 00:00:00', '2026-02-25 00:00:00');
            "#,
        ))
        .await;

    let response = list_json_index(&server, None).await;
    assert_eq!(response.items.len(), 1);
    assert_eq!(response.items[0].title, "Rust - The Book");
}
