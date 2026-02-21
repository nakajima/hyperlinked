use sea_orm_migration::prelude::*;

const JOBS_BY_HYPERLINK_CREATED_AT_INDEX: &str =
    "idx_hyperlink_processing_job_hyperlink_id_created_at";
const JOBS_BY_HYPERLINK_STATE_CREATED_AT_INDEX: &str =
    "idx_hyperlink_processing_job_hyperlink_id_state_created_at";
const SNAPSHOTS_BY_HYPERLINK_CREATED_AT_INDEX: &str =
    "idx_hyperlink_snapshot_hyperlink_id_created_at";
const SNAPSHOTS_BY_JOB_ID_INDEX: &str = "idx_hyperlink_snapshot_job_id";
const ERRORS_BY_HYPERLINK_CREATED_AT_INDEX: &str =
    "idx_hyperlink_processing_error_hyperlink_id_created_at";
const ERRORS_BY_HYPERLINK_ATTEMPT_INDEX: &str =
    "idx_hyperlink_processing_error_hyperlink_id_attempt";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(HyperlinkProcessingJob::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(HyperlinkProcessingJob::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkProcessingJob::HyperlinkId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkProcessingJob::State)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkProcessingJob::ErrorMessage)
                            .text()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkProcessingJob::QueuedAt)
                            .date_time()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkProcessingJob::StartedAt)
                            .date_time()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkProcessingJob::FinishedAt)
                            .date_time()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkProcessingJob::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkProcessingJob::UpdatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-hyperlink-processing-job-hyperlink-id")
                            .from(
                                HyperlinkProcessingJob::Table,
                                HyperlinkProcessingJob::HyperlinkId,
                            )
                            .to(Hyperlink::Table, Hyperlink::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(JOBS_BY_HYPERLINK_CREATED_AT_INDEX)
                    .table(HyperlinkProcessingJob::Table)
                    .col(HyperlinkProcessingJob::HyperlinkId)
                    .col(HyperlinkProcessingJob::CreatedAt)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(JOBS_BY_HYPERLINK_STATE_CREATED_AT_INDEX)
                    .table(HyperlinkProcessingJob::Table)
                    .col(HyperlinkProcessingJob::HyperlinkId)
                    .col(HyperlinkProcessingJob::State)
                    .col(HyperlinkProcessingJob::CreatedAt)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(HyperlinkSnapshot::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(HyperlinkSnapshot::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkSnapshot::HyperlinkId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkSnapshot::JobId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkSnapshot::Payload)
                            .binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkSnapshot::ContentType)
                            .string()
                            .not_null()
                            .default("application/warc"),
                    )
                    .col(
                        ColumnDef::new(HyperlinkSnapshot::SizeBytes)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkSnapshot::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-hyperlink-snapshot-hyperlink-id")
                            .from(HyperlinkSnapshot::Table, HyperlinkSnapshot::HyperlinkId)
                            .to(Hyperlink::Table, Hyperlink::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-hyperlink-snapshot-job-id")
                            .from(HyperlinkSnapshot::Table, HyperlinkSnapshot::JobId)
                            .to(HyperlinkProcessingJob::Table, HyperlinkProcessingJob::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(SNAPSHOTS_BY_HYPERLINK_CREATED_AT_INDEX)
                    .table(HyperlinkSnapshot::Table)
                    .col(HyperlinkSnapshot::HyperlinkId)
                    .col(HyperlinkSnapshot::CreatedAt)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(SNAPSHOTS_BY_JOB_ID_INDEX)
                    .table(HyperlinkSnapshot::Table)
                    .col(HyperlinkSnapshot::JobId)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(HyperlinkProcessingError::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .drop_column(Hyperlink::ProcessingState)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .drop_column(Hyperlink::ProcessingStartedAt)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .drop_column(Hyperlink::ProcessedAt)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .add_column(
                        ColumnDef::new(Hyperlink::ProcessingState)
                            .string()
                            .not_null()
                            .default("waiting"),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .add_column(
                        ColumnDef::new(Hyperlink::ProcessingStartedAt)
                            .date_time()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .add_column(ColumnDef::new(Hyperlink::ProcessedAt).date_time().null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(HyperlinkProcessingError::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(HyperlinkProcessingError::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkProcessingError::HyperlinkId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkProcessingError::Attempt)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkProcessingError::ErrorMessage)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkProcessingError::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-hyperlink-processing-error-hyperlink-id")
                            .from(
                                HyperlinkProcessingError::Table,
                                HyperlinkProcessingError::HyperlinkId,
                            )
                            .to(Hyperlink::Table, Hyperlink::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(ERRORS_BY_HYPERLINK_CREATED_AT_INDEX)
                    .table(HyperlinkProcessingError::Table)
                    .col(HyperlinkProcessingError::HyperlinkId)
                    .col(HyperlinkProcessingError::CreatedAt)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(ERRORS_BY_HYPERLINK_ATTEMPT_INDEX)
                    .table(HyperlinkProcessingError::Table)
                    .col(HyperlinkProcessingError::HyperlinkId)
                    .col(HyperlinkProcessingError::Attempt)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(SNAPSHOTS_BY_JOB_ID_INDEX)
                    .table(HyperlinkSnapshot::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(SNAPSHOTS_BY_HYPERLINK_CREATED_AT_INDEX)
                    .table(HyperlinkSnapshot::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(HyperlinkSnapshot::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(JOBS_BY_HYPERLINK_STATE_CREATED_AT_INDEX)
                    .table(HyperlinkProcessingJob::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(JOBS_BY_HYPERLINK_CREATED_AT_INDEX)
                    .table(HyperlinkProcessingJob::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(HyperlinkProcessingJob::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Hyperlink {
    Table,
    Id,
    ProcessingState,
    ProcessingStartedAt,
    ProcessedAt,
}

#[derive(DeriveIden)]
enum HyperlinkProcessingError {
    Table,
    Id,
    HyperlinkId,
    Attempt,
    ErrorMessage,
    CreatedAt,
}

#[derive(DeriveIden)]
enum HyperlinkProcessingJob {
    Table,
    Id,
    HyperlinkId,
    State,
    ErrorMessage,
    QueuedAt,
    StartedAt,
    FinishedAt,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum HyperlinkSnapshot {
    Table,
    Id,
    HyperlinkId,
    JobId,
    Payload,
    ContentType,
    SizeBytes,
    CreatedAt,
}
