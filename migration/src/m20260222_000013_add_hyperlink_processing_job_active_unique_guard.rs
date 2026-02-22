use sea_orm_migration::{prelude::*, sea_orm::ConnectionTrait};

const ACTIVE_JOB_UNIQUE_INDEX_NAME: &str = "idx_hyperlink_processing_job_active_unique";

const MARK_DUPLICATE_ACTIVE_JOBS_FAILED_SQL: &str = r#"
    UPDATE hyperlink_processing_job
    SET
        state = 'failed',
        error_message = CASE
            WHEN error_message IS NULL OR TRIM(error_message) = ''
                THEN 'marked failed by migration: duplicate active job'
            ELSE error_message
        END,
        finished_at = COALESCE(finished_at, CURRENT_TIMESTAMP),
        updated_at = CURRENT_TIMESTAMP
    WHERE state IN ('queued', 'running')
      AND id NOT IN (
            SELECT MAX(id)
            FROM hyperlink_processing_job
            WHERE state IN ('queued', 'running')
            GROUP BY hyperlink_id, kind
      )
"#;

const CREATE_ACTIVE_JOB_UNIQUE_INDEX_SQL: &str = r#"
    CREATE UNIQUE INDEX IF NOT EXISTS idx_hyperlink_processing_job_active_unique
    ON hyperlink_processing_job (hyperlink_id, kind)
    WHERE state IN ('queued', 'running')
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();
        connection
            .execute_unprepared(MARK_DUPLICATE_ACTIVE_JOBS_FAILED_SQL)
            .await?;
        connection
            .execute_unprepared(CREATE_ACTIVE_JOB_UNIQUE_INDEX_SQL)
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name(ACTIVE_JOB_UNIQUE_INDEX_NAME)
                    .table(HyperlinkProcessingJob::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum HyperlinkProcessingJob {
    Table,
}
