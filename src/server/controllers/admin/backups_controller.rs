use axum::Router;

use crate::server::context::Context;

// Placeholder module for future extraction of backup endpoints from
// `dashboard_controller` while keeping route behavior unchanged in this pass.
pub fn routes() -> Router<Context> {
    Router::new()
}
