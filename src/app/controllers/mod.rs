pub mod admin;
pub mod flash;
pub mod hyperlinks_controller;
pub mod uploads_controller;

use axum::Router;

use crate::server::context::Context;

pub fn routes() -> Router<Context> {
    Router::new()
        .merge(hyperlinks_controller::routes())
        .merge(uploads_controller::routes())
        .merge(admin::routes())
}
