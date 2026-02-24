use sea_orm_migration::prelude::*;

const HYPERLINK_CREATED_AT_ID_INDEX: &str = "idx_hyperlink_created_at_id";
const HYPERLINK_CLICKS_CREATED_AT_ID_INDEX: &str = "idx_hyperlink_clicks_count_created_at_id";
const HYPERLINK_LAST_CLICKED_CREATED_AT_ID_INDEX: &str =
    "idx_hyperlink_last_clicked_at_created_at_id";
const HYPERLINK_DISCOVERY_CLICKS_CREATED_AT_ID_INDEX: &str =
    "idx_hyperlink_discovery_depth_clicks_count_created_at_id";
const HYPERLINK_DISCOVERY_LAST_CLICKED_CREATED_AT_ID_INDEX: &str =
    "idx_hyperlink_discovery_depth_last_clicked_at_created_at_id";
const JOBS_BY_HYPERLINK_CREATED_AT_ID_INDEX: &str =
    "idx_hyperlink_processing_job_hyperlink_id_created_at_id";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_index(
                Index::create()
                    .name(HYPERLINK_CREATED_AT_ID_INDEX)
                    .table(Hyperlink::Table)
                    .col(Hyperlink::CreatedAt)
                    .col(Hyperlink::Id)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(HYPERLINK_CLICKS_CREATED_AT_ID_INDEX)
                    .table(Hyperlink::Table)
                    .col(Hyperlink::ClicksCount)
                    .col(Hyperlink::CreatedAt)
                    .col(Hyperlink::Id)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(HYPERLINK_LAST_CLICKED_CREATED_AT_ID_INDEX)
                    .table(Hyperlink::Table)
                    .col(Hyperlink::LastClickedAt)
                    .col(Hyperlink::CreatedAt)
                    .col(Hyperlink::Id)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(HYPERLINK_DISCOVERY_CLICKS_CREATED_AT_ID_INDEX)
                    .table(Hyperlink::Table)
                    .col(Hyperlink::DiscoveryDepth)
                    .col(Hyperlink::ClicksCount)
                    .col(Hyperlink::CreatedAt)
                    .col(Hyperlink::Id)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(HYPERLINK_DISCOVERY_LAST_CLICKED_CREATED_AT_ID_INDEX)
                    .table(Hyperlink::Table)
                    .col(Hyperlink::DiscoveryDepth)
                    .col(Hyperlink::LastClickedAt)
                    .col(Hyperlink::CreatedAt)
                    .col(Hyperlink::Id)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(JOBS_BY_HYPERLINK_CREATED_AT_ID_INDEX)
                    .table(HyperlinkProcessingJob::Table)
                    .col(HyperlinkProcessingJob::HyperlinkId)
                    .col(HyperlinkProcessingJob::CreatedAt)
                    .col(HyperlinkProcessingJob::Id)
                    .if_not_exists()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name(JOBS_BY_HYPERLINK_CREATED_AT_ID_INDEX)
                    .table(HyperlinkProcessingJob::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(HYPERLINK_DISCOVERY_LAST_CLICKED_CREATED_AT_ID_INDEX)
                    .table(Hyperlink::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(HYPERLINK_DISCOVERY_CLICKS_CREATED_AT_ID_INDEX)
                    .table(Hyperlink::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(HYPERLINK_LAST_CLICKED_CREATED_AT_ID_INDEX)
                    .table(Hyperlink::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(HYPERLINK_CLICKS_CREATED_AT_ID_INDEX)
                    .table(Hyperlink::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(HYPERLINK_CREATED_AT_ID_INDEX)
                    .table(Hyperlink::Table)
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
    DiscoveryDepth,
    CreatedAt,
    ClicksCount,
    LastClickedAt,
}

#[derive(DeriveIden)]
enum HyperlinkProcessingJob {
    Table,
    Id,
    HyperlinkId,
    CreatedAt,
}
