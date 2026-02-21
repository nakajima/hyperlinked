use sea_orm::DatabaseConnection;

use crate::model::hyperlink_processing_job::ProcessingQueueSender;

#[derive(Clone)]
pub struct Context {
    pub connection: DatabaseConnection,
    pub processing_queue: Option<ProcessingQueueSender>,
}
