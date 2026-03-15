use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

const LEGACY_TAG_KEYS: [&str; 14] = [
    "settings.tags.enabled",
    "settings.tags.provider",
    "settings.tags.base_url",
    "settings.tags.api_key",
    "settings.tags.model",
    "settings.tags.auth_header_name",
    "settings.tags.auth_header_prefix",
    "settings.tags.backend_kind",
    "settings.tags.vocabulary_json",
    "settings.tags.topic_vocabulary_json",
    "settings.tags.action_vocabulary_json",
    "settings.tags.topic_taxonomy_json",
    "settings.tags.action_taxonomy_json",
    "settings.tags.auto_approve_ai",
];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let connection = manager.get_connection();

        connection
            .execute(Statement::from_string(
                backend,
                "DELETE FROM hyperlink_artifact WHERE kind = 'tag_meta'".to_string(),
            ))
            .await?;
        connection
            .execute(Statement::from_string(
                backend,
                "DELETE FROM hyperlink_processing_job WHERE kind IN ('tag_classification', 'tag_reclassify')".to_string(),
            ))
            .await?;

        for table in [
            "hyperlink_action_tag",
            "hyperlink_topic_tag",
            "action_tag",
            "topic_tag",
            "hyperlink_tag",
            "tag",
        ] {
            connection
                .execute(Statement::from_string(
                    backend,
                    format!("DROP TABLE IF EXISTS {table}"),
                ))
                .await?;
        }

        for key in LEGACY_TAG_KEYS {
            connection
                .execute(Statement::from_sql_and_values(
                    backend,
                    "DELETE FROM app_kv WHERE key = ?".to_string(),
                    vec![key.into()],
                ))
                .await?;
        }

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
