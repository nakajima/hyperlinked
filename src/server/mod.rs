pub mod admin;
pub mod admin_jobs;
pub mod context;
mod flash;
pub mod graphql;
mod html_layout;
mod hyperlink_fetcher;
#[cfg(test)]
pub(crate) mod test_support;
mod views;

use axum::{Router, response::Redirect, routing::get};
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::services::ServeDir;
use tracing::instrument;

pub mod hyperlinks;

#[instrument(level = tracing::Level::TRACE)]
pub async fn start(host: &str, port: &str) -> Result<(), String> {
    let connection = crate::db::connection::init()
        .await
        .map_err(|err| format!("failed to initialize database connection: {err}"))?;
    let processing_queue = crate::queue::ProcessingQueue::connect(connection.clone()).await?;
    processing_queue.spawn_worker(connection.clone()).await?;
    let _artifact_gc_worker = crate::storage::gc::spawn(connection.clone());

    let jobs_dashboard = lilqueue::dashboard::router_with_control(
        processing_queue.dashboard_db(),
        lilqueue::dashboard::DashboardOptions::default(),
        Arc::new(processing_queue.clone()),
    );
    let state = context::Context {
        connection,
        processing_queue: Some(processing_queue),
    };
    let assets_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/server/assets");
    let app = Router::<context::Context>::new()
        .route("/", get(|| async { Redirect::temporary("/hyperlinks") }))
        .merge(admin::routes())
        .merge(admin_jobs::routes())
        .merge(graphql::routes())
        .merge(hyperlinks::links())
        .nest_service("/jobs", jobs_dashboard.into_service())
        .nest_service("/assets", ServeDir::new(assets_dir))
        .with_state(state);
    let addr = [host, port].join(":");

    tracing::info!("starting server at {}", addr);

    let listener = tokio::net::TcpListener::bind(addr.clone())
        .await
        .map_err(|err| format!("failed to bind {addr}: {err}"))?;
    axum::serve(listener, app)
        .await
        .map_err(|err| format!("server error on {addr}: {err}"))?;
    Ok(())
}
