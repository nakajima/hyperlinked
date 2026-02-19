use sea_orm_migration::{prelude::*, sea_orm::Statement};

#[derive(DeriveMigrationName)]
pub struct Migration;

const URL_UNIQUE_INDEX: &str = "idx_hyperlink_url_unique";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute(Statement::from_string(
                manager.get_database_backend(),
                r#"
                    DELETE FROM hyperlink
                    WHERE id NOT IN (
                        SELECT MIN(id)
                        FROM hyperlink
                        GROUP BY url
                    );
                "#
                .to_string(),
            ))
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(URL_UNIQUE_INDEX)
                    .table(Hyperlink::Table)
                    .col(Hyperlink::URL)
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
                    .name(URL_UNIQUE_INDEX)
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
    URL,
}
