use sea_orm_migration::{prelude::*, sea_orm::ConnectionTrait};

#[derive(DeriveMigrationName)]
pub struct Migration;

const DROP_READABLE_TEXT_INSERT_TRIGGER_SQL: &str =
    "DROP TRIGGER IF EXISTS trg_hyperlink_search_doc_readable_text_ai";
const CREATE_READABLE_TEXT_INSERT_TRIGGER_SQL: &str = r#"
    CREATE TRIGGER IF NOT EXISTS trg_hyperlink_search_doc_readable_text_ai
    AFTER INSERT ON hyperlink_artifact
    WHEN NEW.kind = 'readable_text'
    BEGIN
        INSERT INTO hyperlink_search_doc (hyperlink_id, title, url, readable_text, updated_at)
        SELECT
            h.id,
            h.title,
            h.url,
            CAST(NEW.payload AS TEXT),
            CURRENT_TIMESTAMP
        FROM hyperlink h
        WHERE h.id = NEW.hyperlink_id
        ON CONFLICT(hyperlink_id) DO UPDATE SET
            title = excluded.title,
            url = excluded.url,
            readable_text = excluded.readable_text,
            updated_at = CURRENT_TIMESTAMP;
    END
"#;

const CREATE_ARTIFACT_GC_PENDING_TABLE_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS artifact_gc_pending (
        id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
        storage_path TEXT NOT NULL UNIQUE,
        attempts INTEGER NOT NULL DEFAULT 0,
        last_error TEXT NULL,
        next_attempt_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
        created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
    )
"#;

const CREATE_ARTIFACT_GC_PENDING_NEXT_ATTEMPT_INDEX_SQL: &str = r#"
    CREATE INDEX IF NOT EXISTS idx_artifact_gc_pending_next_attempt_at
    ON artifact_gc_pending (next_attempt_at, id)
"#;

const CREATE_ARTIFACT_GC_TRIGGER_SQL: &str = r#"
    CREATE TRIGGER IF NOT EXISTS trg_artifact_gc_pending_hyperlink_artifact_ad
    AFTER DELETE ON hyperlink_artifact
    WHEN OLD.storage_path IS NOT NULL AND LENGTH(TRIM(OLD.storage_path)) > 0
    BEGIN
        INSERT INTO artifact_gc_pending (
            storage_path,
            attempts,
            last_error,
            next_attempt_at,
            created_at,
            updated_at
        )
        VALUES (
            OLD.storage_path,
            0,
            NULL,
            CURRENT_TIMESTAMP,
            CURRENT_TIMESTAMP,
            CURRENT_TIMESTAMP
        )
        ON CONFLICT(storage_path) DO UPDATE SET
            next_attempt_at = CURRENT_TIMESTAMP,
            updated_at = CURRENT_TIMESTAMP;
    END
"#;

const DROP_ARTIFACT_GC_TRIGGER_SQL: &str =
    "DROP TRIGGER IF EXISTS trg_artifact_gc_pending_hyperlink_artifact_ad";
const DROP_ARTIFACT_GC_PENDING_NEXT_ATTEMPT_INDEX_SQL: &str =
    "DROP INDEX IF EXISTS idx_artifact_gc_pending_next_attempt_at";
const DROP_ARTIFACT_GC_PENDING_TABLE_SQL: &str = "DROP TABLE IF EXISTS artifact_gc_pending";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(HyperlinkArtifact::Table)
                    .add_column(ColumnDef::new(HyperlinkArtifact::StoragePath).string().null())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(HyperlinkArtifact::Table)
                    .add_column(ColumnDef::new(HyperlinkArtifact::StorageBackend).string().null())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(HyperlinkArtifact::Table)
                    .add_column(ColumnDef::new(HyperlinkArtifact::ChecksumSha256).string().null())
                    .to_owned(),
            )
            .await?;

        let connection = manager.get_connection();
        connection
            .execute_unprepared(DROP_READABLE_TEXT_INSERT_TRIGGER_SQL)
            .await?;
        connection
            .execute_unprepared(CREATE_ARTIFACT_GC_PENDING_TABLE_SQL)
            .await?;
        connection
            .execute_unprepared(CREATE_ARTIFACT_GC_PENDING_NEXT_ATTEMPT_INDEX_SQL)
            .await?;
        connection
            .execute_unprepared(CREATE_ARTIFACT_GC_TRIGGER_SQL)
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();
        connection
            .execute_unprepared(DROP_ARTIFACT_GC_TRIGGER_SQL)
            .await?;
        connection
            .execute_unprepared(DROP_ARTIFACT_GC_PENDING_NEXT_ATTEMPT_INDEX_SQL)
            .await?;
        connection
            .execute_unprepared(DROP_ARTIFACT_GC_PENDING_TABLE_SQL)
            .await?;
        connection
            .execute_unprepared(CREATE_READABLE_TEXT_INSERT_TRIGGER_SQL)
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(HyperlinkArtifact::Table)
                    .drop_column(HyperlinkArtifact::ChecksumSha256)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(HyperlinkArtifact::Table)
                    .drop_column(HyperlinkArtifact::StorageBackend)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(HyperlinkArtifact::Table)
                    .drop_column(HyperlinkArtifact::StoragePath)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum HyperlinkArtifact {
    Table,
    StoragePath,
    StorageBackend,
    ChecksumSha256,
}
