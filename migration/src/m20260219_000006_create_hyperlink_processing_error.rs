use sea_orm_migration::prelude::*;

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
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name(ERRORS_BY_HYPERLINK_ATTEMPT_INDEX)
                    .table(HyperlinkProcessingError::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(ERRORS_BY_HYPERLINK_CREATED_AT_INDEX)
                    .table(HyperlinkProcessingError::Table)
                    .if_exists()
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
            .await
    }
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
enum Hyperlink {
    Table,
    Id,
}
