pub mod admin;
pub(crate) mod admin_backup;
pub mod admin_jobs;
mod chromium_diagnostics;
pub mod context;
mod flash;
pub(crate) mod font_diagnostics;
pub mod graphql;
mod html_layout;
mod hyperlink_fetcher;
mod mdns;
#[cfg(test)]
pub(crate) mod test_support;
mod views;

use axum::{Router, response::Redirect, routing::get};
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::services::ServeDir;
use tracing::instrument;

pub mod hyperlinks;
pub use mdns::MdnsOptions;

#[instrument(level = tracing::Level::TRACE)]
pub async fn start(host: &str, port: &str, mdns_options: MdnsOptions) -> Result<(), String> {
    let connection = crate::db::connection::init()
        .await
        .map_err(|err| format!("failed to initialize database connection: {err}"))?;
    match crate::model::hyperlink_processing_job::delete_stale_active_rows(&connection).await {
        Ok(repaired) if repaired > 0 => {
            tracing::info!(repaired, "deleted stale queued/running processing jobs");
        }
        Ok(_) => {}
        Err(err) => {
            tracing::warn!(error = %err, "failed to delete stale queued/running processing jobs");
        }
    }
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
        backup_exports: crate::server::admin_backup::AdminBackupManager::default(),
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

    let _mdns_advertisement = if mdns_options.enabled {
        match port.parse::<u16>() {
            Ok(parsed_port) => match mdns::MdnsAdvertisement::start(&mdns_options, parsed_port) {
                Ok(advertisement) => {
                    if advertisement.is_some() {
                        tracing::info!(
                            "mDNS advertised as {} ({}) on port {}",
                            mdns_options.service_name,
                            mdns_options.service_type,
                            parsed_port
                        );
                    }
                    advertisement
                }
                Err(err) => {
                    tracing::warn!("mDNS advertisement disabled: {err}");
                    None
                }
            },
            Err(err) => {
                tracing::warn!("mDNS advertisement disabled due to invalid port `{port}`: {err}");
                None
            }
        }
    } else {
        None
    };

    tracing::info!("starting server at {}", addr);

    let listener = tokio::net::TcpListener::bind(addr.clone())
        .await
        .map_err(|err| format!("failed to bind {addr}: {err}"))?;
    axum::serve(listener, app)
        .await
        .map_err(|err| format!("server error on {addr}: {err}"))?;
    Ok(())
}
