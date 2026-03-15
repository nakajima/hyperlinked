pub mod admin;
pub mod flash;
pub mod hyperlink_artifacts_controller;
pub mod hyperlinks_controller;
pub mod uploads_controller;

use axum::Router;

use crate::server::context::Context;

pub fn routes() -> Router<Context> {
    let router = Router::new()
        .merge(hyperlinks_controller::routes())
        .merge(uploads_controller::routes())
        .merge(admin::routes());

    #[cfg(not(test))]
    let router = router.merge(hyperlink_artifacts_controller::routes());

    router
}
