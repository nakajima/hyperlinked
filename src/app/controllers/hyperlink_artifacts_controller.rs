use axum::{Router, routing};

use super::hyperlinks_controller;
use crate::server::context::Context;

pub fn routes() -> Router<Context> {
    Router::new()
        .route(
            "/hyperlinks/{id}/artifacts/{kind}",
            routing::get(hyperlinks_controller::download_latest_artifact),
        )
        .route(
            "/hyperlinks/{id}/artifacts/{kind}/inline",
            routing::get(hyperlinks_controller::render_latest_artifact_inline),
        )
        .route(
            "/hyperlinks/{id}/artifacts/pdf_source/preview",
            routing::get(hyperlinks_controller::render_pdf_source_preview),
        )
        .route(
            "/hyperlinks/{id}/artifacts/{kind}/delete",
            routing::post(hyperlinks_controller::delete_artifact_kind),
        )
        .route(
            "/hyperlinks/{id}/artifacts/{kind}/fetch",
            routing::post(hyperlinks_controller::fetch_artifact_kind),
        )
}
