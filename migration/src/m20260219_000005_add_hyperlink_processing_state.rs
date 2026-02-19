use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
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
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .drop_column(Hyperlink::ProcessedAt)
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
                    .drop_column(Hyperlink::ProcessingState)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Hyperlink {
    Table,
    ProcessingState,
    ProcessingStartedAt,
    ProcessedAt,
}
