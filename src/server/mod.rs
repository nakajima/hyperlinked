pub(crate) mod admin_backup;
pub(crate) mod admin_import;
pub(crate) mod chromium_diagnostics;
pub mod context;
pub(crate) mod font_diagnostics;
pub mod graphql;
mod http_logging;
mod mdns;

use axum::{Router, response::Redirect, routing::get};
use axum::{
    body::Body,
    extract::Path,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use include_dir::{Dir, include_dir};
use sea_orm::DatabaseConnection;
use std::path::{Component, Path as StdPath, PathBuf};
use std::sync::{Arc, LazyLock};
use tracing::instrument;

pub use crate::app::controllers;
pub use crate::app::controllers::admin::dashboard_controller as admin;
pub use crate::app::controllers::admin::jobs_controller as admin_jobs;
pub(crate) use crate::app::controllers::flash;
pub use crate::app::controllers::hyperlinks_controller as hyperlinks;
pub(crate) use crate::app::services::hyperlink_fetcher;
pub(crate) use crate::app::views::renderer as views;
pub use mdns::MdnsOptions;

static EMBEDDED_ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/src/server/assets");
static ASSET_ROOTS: LazyLock<Vec<PathBuf>> = LazyLock::new(asset_roots);

#[instrument(level = tracing::Level::TRACE)]
pub async fn start(
    connection: DatabaseConnection,
    host: &str,
    port: &str,
    mdns_options: MdnsOptions,
) -> Result<(), String> {
    match crate::app::models::hyperlink_processing_job::delete_stale_active_rows(&connection).await
    {
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
    let _feed_poller = crate::app::services::feed_poller::spawn(
        connection.clone(),
        Some(processing_queue.clone()),
    );

    let jobs_dashboard = lilqueue::dashboard::router_with_control(
        processing_queue.dashboard_db(),
        lilqueue::dashboard::DashboardOptions::default(),
        Arc::new(processing_queue.clone()),
    );
    let state = context::Context {
        connection,
        processing_queue: Some(processing_queue),
        backup_exports: crate::server::admin_backup::AdminBackupManager::default(),
        backup_imports: crate::server::admin_import::AdminImportManager::default(),
    };
    let app = Router::<context::Context>::new()
        .route("/", get(|| async { Redirect::temporary("/hyperlinks") }))
        .merge(controllers::routes())
        .merge(graphql::routes())
        .nest_service("/jobs", jobs_dashboard.into_service())
        .route("/favicon.ico", get(serve_favicon))
        .route("/assets/{*path}", get(serve_asset))
        .layer(axum::middleware::from_fn(http_logging::log_requests))
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

fn asset_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let source_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/server/assets");
    roots.push(source_root);
    if let Ok(executable_path) = std::env::current_exe()
        && let Some(parent) = executable_path.parent()
    {
        let sibling_assets = parent.join("assets");
        if sibling_assets != roots[0] {
            roots.push(sibling_assets);
        }
    }
    roots
}

async fn serve_asset(Path(requested_path): Path<String>) -> Response {
    let Some(relative_path) = sanitize_asset_path(&requested_path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    serve_asset_by_relative_path(relative_path).await
}

async fn serve_favicon() -> Response {
    serve_asset_by_relative_path(PathBuf::from("favicon.png")).await
}

async fn serve_asset_by_relative_path(relative_path: PathBuf) -> Response {
    for root in ASSET_ROOTS.iter() {
        let candidate = root.join(&relative_path);
        if let Ok(bytes) = tokio::fs::read(&candidate).await {
            let content_type = content_type_for_path(relative_path.to_str().unwrap_or(""));
            return asset_response(bytes, content_type);
        }
    }

    let embedded_lookup = relative_path.to_string_lossy();
    let Some(file) = EMBEDDED_ASSETS.get_file(embedded_lookup.as_ref()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = content_type_for_path(&embedded_lookup);
    asset_response(file.contents().to_vec(), content_type)
}

fn sanitize_asset_path(value: &str) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in StdPath::new(value).components() {
        match component {
            Component::Normal(segment) => normalized.push(segment),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if normalized.as_os_str().is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn content_type_for_path(path: &str) -> &'static str {
    let extension = StdPath::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "css" => "text/css; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "svg" => "image/svg+xml",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "json" => "application/json; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn asset_response(bytes: Vec<u8>, content_type: &'static str) -> Response {
    let mut response = Response::new(Body::from(bytes));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}
#[cfg(test)]
#[path = "../../tests/unit/server.rs"]
mod tests;
