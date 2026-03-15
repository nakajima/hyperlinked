use axum::{Router, routing};

use super::dashboard_controller;
use crate::server::context::Context;

pub fn routes() -> Router<Context> {
    Router::new()
        .route(
            "/admin/clear-queue",
            routing::post(dashboard_controller::clear_queue),
        )
        .route(
            "/admin/pause-queue",
            routing::post(dashboard_controller::pause_queue),
        )
        .route(
            "/admin/resume-queue",
            routing::post(dashboard_controller::resume_queue),
        )
}
