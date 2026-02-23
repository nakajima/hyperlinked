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
                    .add_column(ColumnDef::new(Hyperlink::OgTitle).string().null())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .add_column(ColumnDef::new(Hyperlink::OgDescription).text().null())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .add_column(ColumnDef::new(Hyperlink::OgType).string().null())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .add_column(ColumnDef::new(Hyperlink::OgUrl).string().null())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .add_column(ColumnDef::new(Hyperlink::OgImageUrl).string().null())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .add_column(ColumnDef::new(Hyperlink::OgSiteName).string().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .drop_column(Hyperlink::OgSiteName)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .drop_column(Hyperlink::OgImageUrl)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .drop_column(Hyperlink::OgUrl)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .drop_column(Hyperlink::OgType)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .drop_column(Hyperlink::OgDescription)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .drop_column(Hyperlink::OgTitle)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Hyperlink {
    Table,
    OgTitle,
    OgDescription,
    OgType,
    OgUrl,
    OgImageUrl,
    OgSiteName,
}
