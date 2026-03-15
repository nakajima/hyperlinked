use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::Statement;

const TOPIC_TAG_NAME_KEY_UNIQUE_INDEX: &str = "idx_topic_tag_name_key_unique";
const ACTION_TAG_NAME_KEY_UNIQUE_INDEX: &str = "idx_action_tag_name_key_unique";
const TOPIC_TAG_PATH_KEY_UNIQUE_INDEX: &str = "idx_topic_tag_path_key_unique";
const ACTION_TAG_PATH_KEY_UNIQUE_INDEX: &str = "idx_action_tag_path_key_unique";
const TOPIC_TAG_PARENT_INDEX: &str = "idx_topic_tag_parent_topic_tag_id";
const ACTION_TAG_PARENT_INDEX: &str = "idx_action_tag_parent_action_tag_id";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(TopicTag::Table)
                    .add_column(ColumnDef::new(TopicTag::ParentTopicTagId).integer().null())
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(TopicTag::Table)
                    .add_column(
                        ColumnDef::new(TopicTag::Path)
                            .string()
                            .not_null()
                            .default(""),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(TopicTag::Table)
                    .add_column(
                        ColumnDef::new(TopicTag::PathKey)
                            .string()
                            .not_null()
                            .default(""),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(TopicTag::Table)
                    .add_column(
                        ColumnDef::new(TopicTag::Depth)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(ActionTag::Table)
                    .add_column(
                        ColumnDef::new(ActionTag::ParentActionTagId)
                            .integer()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ActionTag::Table)
                    .add_column(
                        ColumnDef::new(ActionTag::Path)
                            .string()
                            .not_null()
                            .default(""),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ActionTag::Table)
                    .add_column(
                        ColumnDef::new(ActionTag::PathKey)
                            .string()
                            .not_null()
                            .default(""),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ActionTag::Table)
                    .add_column(
                        ColumnDef::new(ActionTag::Depth)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await?;

        let backend = manager.get_database_backend();
        manager
            .get_connection()
            .execute(Statement::from_string(
                backend,
                r#"
                UPDATE topic_tag
                SET parent_topic_tag_id = NULL,
                    path = name,
                    path_key = name_key,
                    depth = 0
                "#
                .to_string(),
            ))
            .await?;
        manager
            .get_connection()
            .execute(Statement::from_string(
                backend,
                r#"
                UPDATE action_tag
                SET parent_action_tag_id = NULL,
                    path = name,
                    path_key = name_key,
                    depth = 0
                "#
                .to_string(),
            ))
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name(TOPIC_TAG_NAME_KEY_UNIQUE_INDEX)
                    .table(TopicTag::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .name(ACTION_TAG_NAME_KEY_UNIQUE_INDEX)
                    .table(ActionTag::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(TOPIC_TAG_PATH_KEY_UNIQUE_INDEX)
                    .table(TopicTag::Table)
                    .col(TopicTag::PathKey)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name(ACTION_TAG_PATH_KEY_UNIQUE_INDEX)
                    .table(ActionTag::Table)
                    .col(ActionTag::PathKey)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name(TOPIC_TAG_PARENT_INDEX)
                    .table(TopicTag::Table)
                    .col(TopicTag::ParentTopicTagId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name(ACTION_TAG_PARENT_INDEX)
                    .table(ActionTag::Table)
                    .col(ActionTag::ParentActionTagId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name(TOPIC_TAG_PARENT_INDEX)
                    .table(TopicTag::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .name(ACTION_TAG_PARENT_INDEX)
                    .table(ActionTag::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .name(TOPIC_TAG_PATH_KEY_UNIQUE_INDEX)
                    .table(TopicTag::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .name(ACTION_TAG_PATH_KEY_UNIQUE_INDEX)
                    .table(ActionTag::Table)
                    .if_exists()
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
            .alter_table(
                Table::alter()
                    .table(TopicTag::Table)
                    .drop_column(TopicTag::ParentTopicTagId)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ActionTag::Table)
                    .drop_column(ActionTag::ParentActionTagId)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(TopicTag::Table)
                    .drop_column(TopicTag::Path)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(TopicTag::Table)
                    .drop_column(TopicTag::PathKey)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(TopicTag::Table)
                    .drop_column(TopicTag::Depth)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ActionTag::Table)
                    .drop_column(ActionTag::Path)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ActionTag::Table)
                    .drop_column(ActionTag::PathKey)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ActionTag::Table)
                    .drop_column(ActionTag::Depth)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum TopicTag {
    Table,
    NameKey,
    ParentTopicTagId,
    Path,
    PathKey,
    Depth,
}

#[derive(DeriveIden)]
enum ActionTag {
    Table,
    NameKey,
    ParentActionTagId,
    Path,
    PathKey,
    Depth,
}
