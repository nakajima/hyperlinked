use sea_orm_migration::prelude::*;

const ARTIFACTS_BY_HYPERLINK_KIND_CREATED_AT_INDEX: &str =
    "idx_hyperlink_artifact_hyperlink_id_kind_created_at";
const ARTIFACTS_BY_JOB_KIND_INDEX: &str = "idx_hyperlink_artifact_job_id_kind";
const SNAPSHOTS_BY_HYPERLINK_CREATED_AT_INDEX: &str =
    "idx_hyperlink_snapshot_hyperlink_id_created_at";
const SNAPSHOTS_BY_JOB_ID_INDEX: &str = "idx_hyperlink_snapshot_job_id";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(HyperlinkProcessingJob::Table)
                    .add_column(
                        ColumnDef::new(HyperlinkProcessingJob::Kind)
                            .string()
                            .not_null()
                            .default("snapshot"),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(HyperlinkArtifact::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(HyperlinkArtifact::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkArtifact::HyperlinkId)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(HyperlinkArtifact::JobId).integer().null())
                    .col(ColumnDef::new(HyperlinkArtifact::Kind).string().not_null())
                    .col(
                        ColumnDef::new(HyperlinkArtifact::Payload)
                            .binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkArtifact::ContentType)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkArtifact::SizeBytes)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkArtifact::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-hyperlink-artifact-hyperlink-id")
                            .from(HyperlinkArtifact::Table, HyperlinkArtifact::HyperlinkId)
                            .to(Hyperlink::Table, Hyperlink::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-hyperlink-artifact-job-id")
                            .from(HyperlinkArtifact::Table, HyperlinkArtifact::JobId)
                            .to(HyperlinkProcessingJob::Table, HyperlinkProcessingJob::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(ARTIFACTS_BY_HYPERLINK_KIND_CREATED_AT_INDEX)
                    .table(HyperlinkArtifact::Table)
                    .col(HyperlinkArtifact::HyperlinkId)
                    .col(HyperlinkArtifact::Kind)
                    .col(HyperlinkArtifact::CreatedAt)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(ARTIFACTS_BY_JOB_KIND_INDEX)
                    .table(HyperlinkArtifact::Table)
                    .col(HyperlinkArtifact::JobId)
                    .col(HyperlinkArtifact::Kind)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                r#"
                INSERT INTO hyperlink_artifact (
                    hyperlink_id,
                    job_id,
                    kind,
                    payload,
                    content_type,
                    size_bytes,
                    created_at
                )
                SELECT
                    hyperlink_id,
                    job_id,
                    'snapshot_warc',
                    payload,
                    content_type,
                    size_bytes,
                    created_at
                FROM hyperlink_snapshot
                "#,
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
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
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
            .get_connection()
            .execute_unprepared(
                r#"
                INSERT INTO hyperlink_snapshot (
                    hyperlink_id,
                    job_id,
                    payload,
                    content_type,
                    size_bytes,
                    created_at
                )
                SELECT
                    hyperlink_id,
                    job_id,
                    payload,
                    content_type,
                    size_bytes,
                    created_at
                FROM hyperlink_artifact
                WHERE kind = 'snapshot_warc' AND job_id IS NOT NULL
                "#,
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(ARTIFACTS_BY_JOB_KIND_INDEX)
                    .table(HyperlinkArtifact::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(ARTIFACTS_BY_HYPERLINK_KIND_CREATED_AT_INDEX)
                    .table(HyperlinkArtifact::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(HyperlinkArtifact::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(HyperlinkProcessingJob::Table)
                    .drop_column(HyperlinkProcessingJob::Kind)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Hyperlink {
    Table,
    Id,
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

#[derive(DeriveIden)]
enum HyperlinkArtifact {
    Table,
    Id,
    HyperlinkId,
    JobId,
    Kind,
    Payload,
    ContentType,
    SizeBytes,
    CreatedAt,
}

#[derive(DeriveIden)]
enum HyperlinkProcessingJob {
    Table,
    Id,
    Kind,
}
