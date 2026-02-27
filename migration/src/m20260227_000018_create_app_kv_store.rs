use sea_orm_migration::{prelude::*, sea_orm::ConnectionTrait};

#[derive(DeriveMigrationName)]
pub struct Migration;

const CREATE_APP_KV_TABLE_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS app_kv (
        key TEXT NOT NULL PRIMARY KEY,
        value TEXT NOT NULL,
        updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
    )
"#;

const DROP_APP_KV_TABLE_SQL: &str = "DROP TABLE IF EXISTS app_kv";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(CREATE_APP_KV_TABLE_SQL)
            .await
            .map(|_| ())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(DROP_APP_KV_TABLE_SQL)
            .await
            .map(|_| ())
    }
}
