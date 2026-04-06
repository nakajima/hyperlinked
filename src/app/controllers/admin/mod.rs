pub mod backups_controller;
pub mod dashboard_controller;
pub mod feeds_controller;
pub mod imports_controller;
pub mod jobs_controller;
pub mod queue_controller;

use axum::Router;

use crate::server::context::Context;

pub fn routes() -> Router<Context> {
    let router = Router::new()
        .merge(dashboard_controller::routes())
        .merge(jobs_controller::routes())
        .merge(feeds_controller::routes());

    #[cfg(not(test))]
    let router = router
        .merge(backups_controller::routes())
        .merge(imports_controller::routes())
        .merge(queue_controller::routes());

    router
}
