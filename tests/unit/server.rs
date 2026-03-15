use super::*;
use axum::body;
use axum::http::header::CONTENT_TYPE;

#[test]
fn embedded_assets_include_core_files() {
    assert!(EMBEDDED_ASSETS.get_file("app.css").is_some());
    assert!(EMBEDDED_ASSETS.get_file("fonts.css").is_some());
    assert!(EMBEDDED_ASSETS.get_file("app.js").is_some());
}

#[test]
fn sanitize_asset_path_rejects_parent_segments() {
    assert!(sanitize_asset_path("../app.css").is_none());
    assert!(sanitize_asset_path("Barlow/../../app.css").is_none());
    assert!(sanitize_asset_path("/app.css").is_none());
}

#[test]
fn sanitize_asset_path_accepts_safe_nested_paths() {
    let path = sanitize_asset_path("Barlow/Barlow-Regular.woff2")
        .expect("safe asset path should be accepted");
    assert_eq!(path, PathBuf::from("Barlow/Barlow-Regular.woff2"));
}

#[tokio::test]
async fn favicon_route_serves_png_content() {
    let response = serve_favicon().await;
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    assert_eq!(content_type, Some("image/png"));

    let body = body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("favicon body should be readable");
    assert!(!body.is_empty());
}
