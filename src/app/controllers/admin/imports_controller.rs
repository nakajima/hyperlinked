use axum::{Router, extract::DefaultBodyLimit, routing};

use super::dashboard_controller;
use crate::server::context::Context;

pub fn routes() -> Router<Context> {
    Router::new()
        .route(
            "/admin/import",
            routing::post(dashboard_controller::import_hyperlinks)
                .layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/admin/import/cancel",
            routing::post(dashboard_controller::cancel_backup_import),
        )
}
