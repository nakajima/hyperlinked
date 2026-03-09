use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::Statement;

const TOPIC_TAG_NAME_KEY_UNIQUE_INDEX: &str = "idx_topic_tag_name_key_unique";
const HYPERLINK_TOPIC_TAG_UNIQUE_INDEX: &str =
    "idx_hyperlink_topic_tag_hyperlink_id_topic_tag_id_unique";
const HYPERLINK_TOPIC_TAG_TAG_ID_INDEX: &str = "idx_hyperlink_topic_tag_topic_tag_id";
const HYPERLINK_TOPIC_TAG_HYPERLINK_SOURCE_INDEX: &str =
    "idx_hyperlink_topic_tag_hyperlink_id_source";
const HYPERLINK_TOPIC_TAG_HYPERLINK_RANK_INDEX: &str =
    "idx_hyperlink_topic_tag_hyperlink_id_rank_index";

const ACTION_TAG_NAME_KEY_UNIQUE_INDEX: &str = "idx_action_tag_name_key_unique";
const HYPERLINK_ACTION_TAG_UNIQUE_INDEX: &str =
    "idx_hyperlink_action_tag_hyperlink_id_action_tag_id_unique";
const HYPERLINK_ACTION_TAG_TAG_ID_INDEX: &str = "idx_hyperlink_action_tag_action_tag_id";
const HYPERLINK_ACTION_TAG_HYPERLINK_SOURCE_INDEX: &str =
    "idx_hyperlink_action_tag_hyperlink_id_source";
const HYPERLINK_ACTION_TAG_HYPERLINK_RANK_INDEX: &str =
    "idx_hyperlink_action_tag_hyperlink_id_rank_index";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(TopicTag::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TopicTag::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(TopicTag::Name).string().not_null())
                    .col(ColumnDef::new(TopicTag::NameKey).string().not_null())
                    .col(
                        ColumnDef::new(TopicTag::State)
                            .string_len(16)
                            .not_null()
                            .default("AI_PENDING"),
                    )
                    .col(ColumnDef::new(TopicTag::CreatedAt).date_time().not_null())
                    .col(ColumnDef::new(TopicTag::UpdatedAt).date_time().not_null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(TOPIC_TAG_NAME_KEY_UNIQUE_INDEX)
                    .table(TopicTag::Table)
                    .col(TopicTag::NameKey)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(HyperlinkTopicTag::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(HyperlinkTopicTag::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkTopicTag::HyperlinkId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkTopicTag::TopicTagId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkTopicTag::Source)
                            .string_len(8)
                            .not_null()
                            .default("AI"),
                    )
                    .col(
                        ColumnDef::new(HyperlinkTopicTag::Confidence)
                            .float()
                            .not_null()
                            .default(0.0),
                    )
                    .col(
                        ColumnDef::new(HyperlinkTopicTag::RankIndex)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(HyperlinkTopicTag::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkTopicTag::UpdatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-hyperlink-topic-tag-hyperlink-id")
                            .from(HyperlinkTopicTag::Table, HyperlinkTopicTag::HyperlinkId)
                            .to(Hyperlink::Table, Hyperlink::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-hyperlink-topic-tag-topic-tag-id")
                            .from(HyperlinkTopicTag::Table, HyperlinkTopicTag::TopicTagId)
                            .to(TopicTag::Table, TopicTag::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(HYPERLINK_TOPIC_TAG_UNIQUE_INDEX)
                    .table(HyperlinkTopicTag::Table)
                    .col(HyperlinkTopicTag::HyperlinkId)
                    .col(HyperlinkTopicTag::TopicTagId)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(HYPERLINK_TOPIC_TAG_TAG_ID_INDEX)
                    .table(HyperlinkTopicTag::Table)
                    .col(HyperlinkTopicTag::TopicTagId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(HYPERLINK_TOPIC_TAG_HYPERLINK_SOURCE_INDEX)
                    .table(HyperlinkTopicTag::Table)
                    .col(HyperlinkTopicTag::HyperlinkId)
                    .col(HyperlinkTopicTag::Source)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(HYPERLINK_TOPIC_TAG_HYPERLINK_RANK_INDEX)
                    .table(HyperlinkTopicTag::Table)
                    .col(HyperlinkTopicTag::HyperlinkId)
                    .col(HyperlinkTopicTag::RankIndex)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(ActionTag::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ActionTag::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ActionTag::Name).string().not_null())
                    .col(ColumnDef::new(ActionTag::NameKey).string().not_null())
                    .col(
                        ColumnDef::new(ActionTag::State)
                            .string_len(16)
                            .not_null()
                            .default("AI_PENDING"),
                    )
                    .col(ColumnDef::new(ActionTag::CreatedAt).date_time().not_null())
                    .col(ColumnDef::new(ActionTag::UpdatedAt).date_time().not_null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(ACTION_TAG_NAME_KEY_UNIQUE_INDEX)
                    .table(ActionTag::Table)
                    .col(ActionTag::NameKey)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(HyperlinkActionTag::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(HyperlinkActionTag::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkActionTag::HyperlinkId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkActionTag::ActionTagId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkActionTag::Source)
                            .string_len(8)
                            .not_null()
                            .default("AI"),
                    )
                    .col(
                        ColumnDef::new(HyperlinkActionTag::Confidence)
                            .float()
                            .not_null()
                            .default(0.0),
                    )
                    .col(
                        ColumnDef::new(HyperlinkActionTag::RankIndex)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(HyperlinkActionTag::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(HyperlinkActionTag::UpdatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-hyperlink-action-tag-hyperlink-id")
                            .from(HyperlinkActionTag::Table, HyperlinkActionTag::HyperlinkId)
                            .to(Hyperlink::Table, Hyperlink::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-hyperlink-action-tag-action-tag-id")
                            .from(HyperlinkActionTag::Table, HyperlinkActionTag::ActionTagId)
                            .to(ActionTag::Table, ActionTag::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(HYPERLINK_ACTION_TAG_UNIQUE_INDEX)
                    .table(HyperlinkActionTag::Table)
                    .col(HyperlinkActionTag::HyperlinkId)
                    .col(HyperlinkActionTag::ActionTagId)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(HYPERLINK_ACTION_TAG_TAG_ID_INDEX)
                    .table(HyperlinkActionTag::Table)
                    .col(HyperlinkActionTag::ActionTagId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(HYPERLINK_ACTION_TAG_HYPERLINK_SOURCE_INDEX)
                    .table(HyperlinkActionTag::Table)
                    .col(HyperlinkActionTag::HyperlinkId)
                    .col(HyperlinkActionTag::Source)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(HYPERLINK_ACTION_TAG_HYPERLINK_RANK_INDEX)
                    .table(HyperlinkActionTag::Table)
                    .col(HyperlinkActionTag::HyperlinkId)
                    .col(HyperlinkActionTag::RankIndex)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        let backend = manager.get_database_backend();
        manager
            .get_connection()
            .execute(Statement::from_string(
                backend,
                r#"
                INSERT INTO action_tag (id, name, name_key, state, created_at, updated_at)
                SELECT id, lower(name), lower(name_key), state, created_at, updated_at
                FROM tag
                "#
                .to_string(),
            ))
            .await?;

        manager
            .get_connection()
            .execute(Statement::from_string(
                backend,
                r#"
                INSERT INTO hyperlink_action_tag
                    (id, hyperlink_id, action_tag_id, source, confidence, rank_index, created_at, updated_at)
                SELECT id, hyperlink_id, tag_id, source, 0.0, 0, created_at, updated_at
                FROM hyperlink_tag
                "#
                .to_string(),
            ))
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name(HYPERLINK_ACTION_TAG_HYPERLINK_RANK_INDEX)
                    .table(HyperlinkActionTag::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(HYPERLINK_ACTION_TAG_HYPERLINK_SOURCE_INDEX)
                    .table(HyperlinkActionTag::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(HYPERLINK_ACTION_TAG_TAG_ID_INDEX)
                    .table(HyperlinkActionTag::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(HYPERLINK_ACTION_TAG_UNIQUE_INDEX)
                    .table(HyperlinkActionTag::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(HyperlinkActionTag::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(ACTION_TAG_NAME_KEY_UNIQUE_INDEX)
                    .table(ActionTag::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(ActionTag::Table).if_exists().to_owned())
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(HYPERLINK_TOPIC_TAG_HYPERLINK_RANK_INDEX)
                    .table(HyperlinkTopicTag::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(HYPERLINK_TOPIC_TAG_HYPERLINK_SOURCE_INDEX)
                    .table(HyperlinkTopicTag::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(HYPERLINK_TOPIC_TAG_TAG_ID_INDEX)
                    .table(HyperlinkTopicTag::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(HYPERLINK_TOPIC_TAG_UNIQUE_INDEX)
                    .table(HyperlinkTopicTag::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(HyperlinkTopicTag::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(TOPIC_TAG_NAME_KEY_UNIQUE_INDEX)
                    .table(TopicTag::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(TopicTag::Table).if_exists().to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Hyperlink {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum TopicTag {
    Table,
    Id,
    Name,
    NameKey,
    State,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum HyperlinkTopicTag {
    Table,
    Id,
    HyperlinkId,
    TopicTagId,
    Source,
    Confidence,
    RankIndex,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum ActionTag {
    Table,
    Id,
    Name,
    NameKey,
    State,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum HyperlinkActionTag {
    Table,
    Id,
    HyperlinkId,
    ActionTagId,
    Source,
    Confidence,
    RankIndex,
    CreatedAt,
    UpdatedAt,
}
