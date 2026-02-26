use sea_orm_migration::prelude::*;

const ROOT_LISTING_INDEX: &str = "idx_hyperlink_discovery_depth_created_at";
const RELATION_PARENT_CREATED_AT_INDEX: &str =
    "idx_hyperlink_relation_parent_hyperlink_id_created_at";
const RELATION_CHILD_INDEX: &str = "idx_hyperlink_relation_child_hyperlink_id";
const RELATION_PARENT_CHILD_UNIQUE_INDEX: &str = "idx_hyperlink_relation_parent_child_unique";

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
                        ColumnDef::new(Hyperlink::DiscoveryDepth)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(ROOT_LISTING_INDEX)
                    .table(Hyperlink::Table)
                    .col(Hyperlink::DiscoveryDepth)
                    .col(Hyperlink::CreatedAt)
                    .col(Hyperlink::Id)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(HyperlinkRelation::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(HyperlinkRelation::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkRelation::ParentHyperlinkId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkRelation::ChildHyperlinkId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkRelation::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-hyperlink-relation-parent-hyperlink-id")
                            .from(
                                HyperlinkRelation::Table,
                                HyperlinkRelation::ParentHyperlinkId,
                            )
                            .to(Hyperlink::Table, Hyperlink::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-hyperlink-relation-child-hyperlink-id")
                            .from(
                                HyperlinkRelation::Table,
                                HyperlinkRelation::ChildHyperlinkId,
                            )
                            .to(Hyperlink::Table, Hyperlink::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .check(
                        Expr::col(HyperlinkRelation::ParentHyperlinkId)
                            .ne(Expr::col(HyperlinkRelation::ChildHyperlinkId)),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(RELATION_PARENT_CREATED_AT_INDEX)
                    .table(HyperlinkRelation::Table)
                    .col(HyperlinkRelation::ParentHyperlinkId)
                    .col(HyperlinkRelation::CreatedAt)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(RELATION_CHILD_INDEX)
                    .table(HyperlinkRelation::Table)
                    .col(HyperlinkRelation::ChildHyperlinkId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(RELATION_PARENT_CHILD_UNIQUE_INDEX)
                    .table(HyperlinkRelation::Table)
                    .col(HyperlinkRelation::ParentHyperlinkId)
                    .col(HyperlinkRelation::ChildHyperlinkId)
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
                    .name(RELATION_PARENT_CHILD_UNIQUE_INDEX)
                    .table(HyperlinkRelation::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(RELATION_CHILD_INDEX)
                    .table(HyperlinkRelation::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(RELATION_PARENT_CREATED_AT_INDEX)
                    .table(HyperlinkRelation::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(HyperlinkRelation::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(ROOT_LISTING_INDEX)
                    .table(Hyperlink::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Hyperlink::Table)
                    .drop_column(Hyperlink::DiscoveryDepth)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Hyperlink {
    Table,
    Id,
    CreatedAt,
    DiscoveryDepth,
}

#[derive(DeriveIden)]
enum HyperlinkRelation {
    Table,
    Id,
    ParentHyperlinkId,
    ChildHyperlinkId,
    CreatedAt,
}
