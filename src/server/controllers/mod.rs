pub mod admin;
pub mod hyperlink_artifacts_controller;
pub mod hyperlinks_controller;

use axum::Router;

use crate::server::context::Context;

pub fn routes() -> Router<Context> {
    Router::new()
        .merge(hyperlinks_controller::routes())
        .merge(hyperlink_artifacts_controller::routes())
        .merge(admin::routes())
}
