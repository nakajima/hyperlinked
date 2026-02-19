use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Hyperlink::Table)
                    .if_not_exists()
                    .col(pk_auto(Hyperlink::Id))
                    .col(string(Hyperlink::Title))
                    .col(string(Hyperlink::URL))
                    .col(date_time(Hyperlink::CreatedAt))
                    .col(date_time(Hyperlink::UpdatedAt))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Replace the sample below with your own migration scripts
        manager
            .drop_table(Table::drop().table(Hyperlink::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Hyperlink {
    Table,
    Id,
    Title,
    URL,
    CreatedAt,
    UpdatedAt,
}
