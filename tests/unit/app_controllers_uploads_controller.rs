use super::*;
use axum::{Router, http::StatusCode};
use axum_test::multipart::{MultipartForm, Part};

use crate::{server::context::Context, test_support};

async fn new_server(seed_sql: &str) -> axum_test::TestServer {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_schema(&connection).await;
    if !seed_sql.trim().is_empty() {
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
    axum_test::TestServer::new(app).expect("test server should initialize")
}

#[tokio::test]
async fn create_upload_persists_pdf_hyperlink() {
    let server = new_server("").await;

    let multipart = MultipartForm::new()
        .add_text("upload_type", "pdf")
        .add_part(
            "file",
            Part::bytes(b"%PDF-1.4\n%abc".to_vec())
                .file_name("paper.pdf")
                .mime_type("application/pdf"),
        );

    let response = server.post("/uploads").multipart(multipart).await;
    response.assert_status(StatusCode::CREATED);

    let payload: serde_json::Value = response.json();
    assert_eq!(payload["url"], format!("{UPLOADS_PREFIX}/1/paper.pdf"));

    let download = server.get("/uploads/1/paper.pdf").await;
    download.assert_status_ok();
    download.assert_header("content-type", "application/pdf");
}

#[tokio::test]
async fn create_upload_reuses_same_hash_and_filename() {
    let server = new_server("").await;

    let build_multipart = || {
        MultipartForm::new()
            .add_text("upload_type", "pdf")
            .add_part(
                "file",
                Part::bytes(b"%PDF-1.4\n%abc".to_vec())
                    .file_name("paper.pdf")
                    .mime_type("application/pdf"),
            )
    };

    let first = server.post("/uploads").multipart(build_multipart()).await;
    first.assert_status(StatusCode::CREATED);
    let second = server.post("/uploads").multipart(build_multipart()).await;
    second.assert_status(StatusCode::OK);

    let first_payload: serde_json::Value = first.json();
    let second_payload: serde_json::Value = second.json();
    assert_eq!(first_payload["id"], second_payload["id"]);
}

#[tokio::test]
async fn create_upload_allows_same_hash_different_filename() {
    let server = new_server("").await;

    let first = server
        .post("/uploads")
        .multipart(
            MultipartForm::new()
                .add_text("upload_type", "pdf")
                .add_part(
                    "file",
                    Part::bytes(b"%PDF-1.4\n%abc".to_vec())
                        .file_name("paper-a.pdf")
                        .mime_type("application/pdf"),
                ),
        )
        .await;
    first.assert_status(StatusCode::CREATED);

    let second = server
        .post("/uploads")
        .multipart(
            MultipartForm::new()
                .add_text("upload_type", "pdf")
                .add_part(
                    "file",
                    Part::bytes(b"%PDF-1.4\n%abc".to_vec())
                        .file_name("paper-b.pdf")
                        .mime_type("application/pdf"),
                ),
        )
        .await;
    second.assert_status(StatusCode::CREATED);

    let first_payload: serde_json::Value = first.json();
    let second_payload: serde_json::Value = second.json();
    assert_ne!(first_payload["id"], second_payload["id"]);
}
