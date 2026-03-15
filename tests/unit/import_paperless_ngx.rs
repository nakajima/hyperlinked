use super::*;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect},
    routing::get,
};
use sea_orm::EntityTrait;
use std::{collections::HashMap, sync::Arc};

#[derive(Clone)]
struct MockPaperlessState {
    pages: Arc<Vec<Vec<Value>>>,
    downloads: Arc<HashMap<i64, (Vec<u8>, String)>>,
}

#[derive(Clone)]
struct RedirectState {
    redirected_page_url: String,
    redirected_download_base_url: String,
}

async fn list_documents(
    State(state): State<MockPaperlessState>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<Value> {
    let page = params
        .get("page")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1);
    let index = page.saturating_sub(1);

    let results = state.pages.get(index).cloned().unwrap_or_default();
    let next = if index + 1 < state.pages.len() {
        Some(format!("/api/documents/?page={}", page + 1))
    } else {
        None
    };

    Json(json!({
        "count": state.pages.iter().map(Vec::len).sum::<usize>(),
        "next": next,
        "previous": if page > 1 { Some(format!("/api/documents/?page={}", page - 1)) } else { None },
        "results": results,
    }))
}

async fn download_document(
    Path(id): Path<i64>,
    State(state): State<MockPaperlessState>,
) -> (StatusCode, [(header::HeaderName, HeaderValue); 1], Vec<u8>) {
    let Some((payload, content_type)) = state.downloads.get(&id).cloned() else {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"))],
            b"not found".to_vec(),
        );
    };

    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_str(content_type.as_str())
                .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
        )],
        payload,
    )
}

async fn list_documents_with_redirect(
    Query(params): Query<HashMap<String, String>>,
) -> Json<Value> {
    let page = params
        .get("page")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1);

    if page == 1 {
        return Json(json!({
            "count": 1,
            "next": "/api/documents/page-2-redirect/",
            "previous": null,
            "results": [],
        }));
    }

    Json(json!({
        "count": 1,
        "next": null,
        "previous": "/api/documents/?page=1",
        "results": [],
    }))
}

async fn page_2_redirect(State(state): State<RedirectState>) -> Redirect {
    Redirect::temporary(&state.redirected_page_url)
}

async fn download_redirect(Path(id): Path<i64>, State(state): State<RedirectState>) -> Redirect {
    Redirect::temporary(&format!(
        "{}/api/documents/{id}/download/",
        state.redirected_download_base_url
    ))
}

async fn list_documents_auth_required(
    State(state): State<MockPaperlessState>,
    Query(params): Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let auth_value = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    if auth_value != Some("Token paperless-token") {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "detail": "Authentication credentials were not provided." })),
        )
            .into_response();
    }

    let page = params
        .get("page")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1);
    let index = page.saturating_sub(1);
    let results = state.pages.get(index).cloned().unwrap_or_default();

    Json(json!({
        "count": state.pages.iter().map(Vec::len).sum::<usize>(),
        "next": null,
        "previous": null,
        "results": results,
    }))
    .into_response()
}

async fn download_document_auth_required(
    Path(id): Path<i64>,
    State(state): State<MockPaperlessState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let auth_value = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    if auth_value != Some("Token paperless-token") {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "detail": "Authentication credentials were not provided." })),
        )
            .into_response();
    }

    let Some((payload, content_type)) = state.downloads.get(&id).cloned() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "detail": "Document not found." })),
        )
            .into_response();
    };

    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_str(content_type.as_str())
                .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
        )],
        payload,
    )
        .into_response()
}

async fn start_mock_paperless(
    pages: Vec<Vec<Value>>,
    downloads: HashMap<i64, (Vec<u8>, String)>,
) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route("/api/documents/", get(list_documents))
        .route("/api/documents/{id}/download/", get(download_document))
        .with_state(MockPaperlessState {
            pages: Arc::new(pages),
            downloads: Arc::new(downloads),
        });

    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("listener should bind");
    let addr = listener
        .local_addr()
        .expect("listener should have local addr");

    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("mock paperless server should run");
    });

    (format!("http://{addr}"), handle)
}

async fn start_auth_required_paperless(
    pages: Vec<Vec<Value>>,
    downloads: HashMap<i64, (Vec<u8>, String)>,
) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route("/api/documents/", get(list_documents_auth_required))
        .route(
            "/api/documents/{id}/download/",
            get(download_document_auth_required),
        )
        .with_state(MockPaperlessState {
            pages: Arc::new(pages),
            downloads: Arc::new(downloads),
        });

    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("listener should bind");
    let addr = listener
        .local_addr()
        .expect("listener should have local addr");

    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("mock paperless server should run");
    });

    (format!("http://{addr}"), handle)
}

async fn start_redirecting_front_server(
    redirected_page_url: String,
    redirected_download_base_url: String,
) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route("/api/documents/", get(list_documents_with_redirect))
        .route("/api/documents/page-2-redirect/", get(page_2_redirect))
        .route("/api/documents/{id}/download/", get(download_redirect))
        .with_state(RedirectState {
            redirected_page_url,
            redirected_download_base_url,
        });

    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("listener should bind");
    let addr = listener
        .local_addr()
        .expect("listener should have local addr");

    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("mock paperless server should run");
    });

    (format!("http://{addr}"), handle)
}

async fn new_connection() -> DatabaseConnection {
    let connection = crate::test_support::new_memory_connection().await;
    crate::test_support::initialize_hyperlinks_schema(&connection).await;
    connection
}

#[tokio::test]
async fn imports_pdf_and_stores_metadata_artifact() {
    let connection = new_connection().await;

    let pages = vec![vec![json!({
        "id": 101,
        "title": "RFC 9114",
        "created": "2026-01-14T20:11:03Z",
        "original_file_name": "rfc-9114.pdf"
    })]];
    let mut downloads = HashMap::new();
    downloads.insert(
        101,
        (
            b"%PDF-1.7\n%imported".to_vec(),
            "application/pdf".to_string(),
        ),
    );

    let (base_url, server_task) = start_mock_paperless(pages, downloads).await;

    let report = import_from_api(
        &connection,
        ImportOptions {
            base_url,
            api_token: "paperless-token".to_string(),
            since: None,
            page_size: Some(50),
            dry_run: false,
        },
        None,
    )
    .await
    .expect("paperless import should succeed");

    server_task.abort();

    assert_eq!(report.summary.scanned, 1);
    assert_eq!(report.summary.imported, 1);
    assert_eq!(report.summary.skipped_duplicate, 0);
    assert_eq!(report.summary.skipped_non_pdf, 0);
    assert_eq!(report.summary.failed, 0);

    let links = hyperlink::Entity::find()
        .all(&connection)
        .await
        .expect("links should load");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].title, "RFC 9114");
    assert!(links[0].url.starts_with("/uploads/1/rfc-9114.pdf"));

    let artifacts = hyperlink_artifact::Entity::find()
        .all(&connection)
        .await
        .expect("artifacts should load");
    assert_eq!(artifacts.len(), 2);
    assert!(
        artifacts
            .iter()
            .any(|row| row.kind == HyperlinkArtifactKind::PdfSource)
    );
    assert!(
        artifacts
            .iter()
            .any(|row| row.kind == HyperlinkArtifactKind::PaperlessMetadata)
    );
}

#[tokio::test]
async fn rerun_skips_duplicate_pdf_by_checksum_and_filename() {
    let connection = new_connection().await;

    let pages = vec![vec![json!({
        "id": 200,
        "title": "Duplicate Test",
        "original_file_name": "dup.pdf"
    })]];
    let payload = b"%PDF-1.5\n%duplicate".to_vec();
    let mut downloads = HashMap::new();
    downloads.insert(200, (payload.clone(), "application/pdf".to_string()));

    let (base_url, server_task) = start_mock_paperless(pages, downloads).await;

    let first = import_from_api(
        &connection,
        ImportOptions {
            base_url: base_url.clone(),
            api_token: "paperless-token".to_string(),
            since: None,
            page_size: None,
            dry_run: false,
        },
        None,
    )
    .await
    .expect("first import should succeed");
    assert_eq!(first.summary.imported, 1);

    let second = import_from_api(
        &connection,
        ImportOptions {
            base_url,
            api_token: "paperless-token".to_string(),
            since: None,
            page_size: None,
            dry_run: false,
        },
        None,
    )
    .await
    .expect("second import should succeed");

    server_task.abort();

    assert_eq!(second.summary.scanned, 1);
    assert_eq!(second.summary.imported, 0);
    assert_eq!(second.summary.skipped_duplicate, 1);
    assert_eq!(second.summary.failed, 0);

    let links = hyperlink::Entity::find()
        .all(&connection)
        .await
        .expect("links should load");
    assert_eq!(links.len(), 1);

    let artifacts = hyperlink_artifact::Entity::find()
        .all(&connection)
        .await
        .expect("artifacts should load");
    assert_eq!(artifacts.len(), 2);
}

#[tokio::test]
async fn skips_non_pdf_downloads() {
    let connection = new_connection().await;

    let pages = vec![vec![json!({
        "id": 333,
        "title": "Not PDF",
        "original_file_name": "not-pdf.pdf"
    })]];
    let mut downloads = HashMap::new();
    downloads.insert(
        333,
        (
            b"this is plain text".to_vec(),
            "text/plain; charset=utf-8".to_string(),
        ),
    );

    let (base_url, server_task) = start_mock_paperless(pages, downloads).await;

    let report = import_from_api(
        &connection,
        ImportOptions {
            base_url,
            api_token: "paperless-token".to_string(),
            since: None,
            page_size: None,
            dry_run: false,
        },
        None,
    )
    .await
    .expect("import should succeed");

    server_task.abort();

    assert_eq!(report.summary.scanned, 1);
    assert_eq!(report.summary.imported, 0);
    assert_eq!(report.summary.skipped_non_pdf, 1);
    assert_eq!(report.summary.failed, 0);

    let links = hyperlink::Entity::find()
        .all(&connection)
        .await
        .expect("links should load");
    assert_eq!(links.len(), 0);
}

#[tokio::test]
async fn handles_download_url_with_leading_slash() {
    let connection = new_connection().await;

    let pages = vec![vec![json!({
        "id": 444,
        "title": "Leading Slash Download",
        "download_url": "/api/documents/444/download/",
        "original_file_name": "leading-slash.pdf"
    })]];
    let mut downloads = HashMap::new();
    downloads.insert(
        444,
        (
            b"%PDF-1.4\n%leading-slash".to_vec(),
            "application/pdf".to_string(),
        ),
    );

    let (base_url, server_task) = start_mock_paperless(pages, downloads).await;

    let report = import_from_api(
        &connection,
        ImportOptions {
            base_url,
            api_token: "paperless-token".to_string(),
            since: None,
            page_size: None,
            dry_run: false,
        },
        None,
    )
    .await
    .expect("import should succeed");

    server_task.abort();

    assert_eq!(report.summary.imported, 1);
    assert_eq!(report.summary.failed, 0);
}

#[tokio::test]
async fn follows_cross_host_next_redirect_and_keeps_auth_header() {
    let connection = new_connection().await;

    let second_pages = vec![vec![json!({
        "id": 555,
        "title": "Redirected Page",
        "original_file_name": "redirected.pdf"
    })]];
    let mut second_downloads = HashMap::new();
    second_downloads.insert(
        555,
        (
            b"%PDF-1.4\n%redirected".to_vec(),
            "application/pdf".to_string(),
        ),
    );
    let (second_base_url, second_server_task) =
        start_auth_required_paperless(second_pages, second_downloads).await;

    let redirected_page_url = format!("{second_base_url}/api/documents/?page=1");
    let (first_base_url, first_server_task) =
        start_redirecting_front_server(redirected_page_url, second_base_url.clone()).await;

    let report = import_from_api(
        &connection,
        ImportOptions {
            base_url: first_base_url,
            api_token: "paperless-token".to_string(),
            since: None,
            page_size: None,
            dry_run: false,
        },
        None,
    )
    .await
    .expect("import should succeed across redirect");

    first_server_task.abort();
    second_server_task.abort();

    assert_eq!(report.summary.scanned, 1);
    assert_eq!(report.summary.imported, 1);
    assert_eq!(report.summary.failed, 0);
}

#[test]
fn parse_datetime_accepts_rfc3339_and_date_only() {
    let with_offset = parse_datetime("2026-03-01T12:40:11+01:00").expect("should parse");
    assert_eq!(with_offset.to_rfc3339(), "2026-03-01T11:40:11+00:00");

    let date_only = parse_datetime("2026-03-01").expect("should parse");
    assert_eq!(date_only.to_rfc3339(), "2026-03-01T00:00:00+00:00");
}
