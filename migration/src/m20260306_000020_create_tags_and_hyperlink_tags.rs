use sea_orm_migration::prelude::*;

const TAG_NAME_KEY_UNIQUE_INDEX: &str = "idx_tag_name_key_unique";
const HYPERLINK_TAG_UNIQUE_INDEX: &str = "idx_hyperlink_tag_hyperlink_id_tag_id_unique";
const HYPERLINK_TAG_TAG_ID_INDEX: &str = "idx_hyperlink_tag_tag_id";
const HYPERLINK_TAG_HYPERLINK_SOURCE_INDEX: &str = "idx_hyperlink_tag_hyperlink_id_source";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Tag::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Tag::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Tag::Name).string().not_null())
                    .col(ColumnDef::new(Tag::NameKey).string().not_null())
                    .col(
                        ColumnDef::new(Tag::State)
                            .string_len(16)
                            .not_null()
                            .default("AI_PENDING"),
                    )
                    .col(ColumnDef::new(Tag::CreatedAt).date_time().not_null())
                    .col(ColumnDef::new(Tag::UpdatedAt).date_time().not_null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(TAG_NAME_KEY_UNIQUE_INDEX)
                    .table(Tag::Table)
                    .col(Tag::NameKey)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(HyperlinkTag::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(HyperlinkTag::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkTag::HyperlinkId)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(HyperlinkTag::TagId).integer().not_null())
                    .col(
                        ColumnDef::new(HyperlinkTag::Source)
                            .string_len(8)
                            .not_null()
                            .default("AI"),
                    )
                    .col(
                        ColumnDef::new(HyperlinkTag::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkTag::UpdatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-hyperlink-tag-hyperlink-id")
                            .from(HyperlinkTag::Table, HyperlinkTag::HyperlinkId)
                            .to(Hyperlink::Table, Hyperlink::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-hyperlink-tag-tag-id")
                            .from(HyperlinkTag::Table, HyperlinkTag::TagId)
                            .to(Tag::Table, Tag::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(HYPERLINK_TAG_UNIQUE_INDEX)
                    .table(HyperlinkTag::Table)
                    .col(HyperlinkTag::HyperlinkId)
                    .col(HyperlinkTag::TagId)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(HYPERLINK_TAG_TAG_ID_INDEX)
                    .table(HyperlinkTag::Table)
                    .col(HyperlinkTag::TagId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(HYPERLINK_TAG_HYPERLINK_SOURCE_INDEX)
                    .table(HyperlinkTag::Table)
                    .col(HyperlinkTag::HyperlinkId)
                    .col(HyperlinkTag::Source)
                    .if_not_exists()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name(HYPERLINK_TAG_HYPERLINK_SOURCE_INDEX)
                    .table(HyperlinkTag::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(HYPERLINK_TAG_TAG_ID_INDEX)
                    .table(HyperlinkTag::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(HYPERLINK_TAG_UNIQUE_INDEX)
                    .table(HyperlinkTag::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(HyperlinkTag::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(TAG_NAME_KEY_UNIQUE_INDEX)
                    .table(Tag::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(Tag::Table).if_exists().to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Hyperlink {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Tag {
    Table,
    Id,
    Name,
    NameKey,
    State,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum HyperlinkTag {
    Table,
    Id,
    HyperlinkId,
    TagId,
    Source,
    CreatedAt,
    UpdatedAt,
}
