use sea_orm::DatabaseConnection;

use crate::model::hyperlink_processing_job::ProcessingQueueSender;
use crate::server::admin_backup::AdminBackupManager;
use crate::server::admin_import::AdminImportManager;
use crate::server::admin_tag_reclassify::AdminTagReclassifyManager;

#[derive(Clone)]
pub struct Context {
    pub connection: DatabaseConnection,
    pub processing_queue: Option<ProcessingQueueSender>,
    pub backup_exports: AdminBackupManager,
    pub backup_imports: AdminImportManager,
    pub tag_reclassify: AdminTagReclassifyManager,
}
