use sea_orm_migration::prelude::*;

const HYPERLINK_TOMBSTONE_UPDATED_AT_HYPERLINK_ID_INDEX: &str =
    "idx_hyperlink_tombstone_updated_at_hyperlink_id";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(HyperlinkTombstone::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(HyperlinkTombstone::HyperlinkId)
                            .integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkTombstone::UpdatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(HYPERLINK_TOMBSTONE_UPDATED_AT_HYPERLINK_ID_INDEX)
                    .table(HyperlinkTombstone::Table)
                    .col(HyperlinkTombstone::UpdatedAt)
                    .col(HyperlinkTombstone::HyperlinkId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name(HYPERLINK_TOMBSTONE_UPDATED_AT_HYPERLINK_ID_INDEX)
                    .table(HyperlinkTombstone::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(HyperlinkTombstone::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum HyperlinkTombstone {
    Table,
    HyperlinkId,
    UpdatedAt,
}
