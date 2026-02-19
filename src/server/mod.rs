pub mod context;
mod html_layout;

use axum::{Router, routing::get};
use std::path::PathBuf;
use tower_http::services::ServeDir;
use tracing::instrument;

pub mod hyperlinks;

#[instrument(level = tracing::Level::TRACE)]
pub async fn start(host: &str, port: &str) {
    let connection = crate::db::connection::init().await.unwrap();
    let processing_queue = crate::processors::worker::spawn(connection.clone());
    let state = context::Context {
        connection,
        processing_queue: Some(processing_queue),
    };
    let assets_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/server/assets");
    let app = Router::<context::Context>::new()
        .route("/", get(|| async { "Hello, World!" }))
        .merge(hyperlinks::links())
        .nest_service("/assets", ServeDir::new(assets_dir))
        .with_state(state);
    let addr = [host, port].join(":");

    tracing::info!("starting server at {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
