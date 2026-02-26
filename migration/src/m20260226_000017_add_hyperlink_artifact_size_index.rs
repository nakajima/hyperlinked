use sea_orm_migration::prelude::*;

const ARTIFACTS_BY_HYPERLINK_SIZE_BYTES_INDEX: &str =
    "idx_hyperlink_artifact_hyperlink_id_size_bytes";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_index(
                Index::create()
                    .name(ARTIFACTS_BY_HYPERLINK_SIZE_BYTES_INDEX)
                    .table(HyperlinkArtifact::Table)
                    .col(HyperlinkArtifact::HyperlinkId)
                    .col(HyperlinkArtifact::SizeBytes)
                    .if_not_exists()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name(ARTIFACTS_BY_HYPERLINK_SIZE_BYTES_INDEX)
                    .table(HyperlinkArtifact::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum HyperlinkArtifact {
    Table,
    HyperlinkId,
    SizeBytes,
}
