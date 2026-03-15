use axum::{Router, routing};

use super::dashboard_controller;
use crate::server::context::Context;

pub fn routes() -> Router<Context> {
    Router::new()
        .route(
            "/admin/export",
            routing::get(dashboard_controller::download_backup_export),
        )
        .route(
            "/admin/export/download",
            routing::get(dashboard_controller::download_backup_export),
        )
        .route(
            "/admin/export/start",
            routing::post(dashboard_controller::start_backup_export),
        )
        .route(
            "/admin/export/cancel",
            routing::post(dashboard_controller::cancel_backup_export),
        )
}
