use sea_orm::DatabaseConnection;

use crate::model::hyperlink_processing_job::ProcessingQueueSender;
use crate::server::admin_backup::AdminBackupManager;

#[derive(Clone)]
pub struct Context {
    pub connection: DatabaseConnection,
    pub processing_queue: Option<ProcessingQueueSender>,
    pub backup_exports: AdminBackupManager,
}
