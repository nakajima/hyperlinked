use sea_orm::DatabaseConnection;

use crate::app::models::hyperlink_processing_job::ProcessingQueueSender;
use crate::server::admin_backup::AdminBackupManager;
use crate::server::admin_import::AdminImportManager;

#[derive(Clone)]
pub struct Context {
    pub connection: DatabaseConnection,
    pub processing_queue: Option<ProcessingQueueSender>,
    pub backup_exports: AdminBackupManager,
    pub backup_imports: AdminImportManager,
}
