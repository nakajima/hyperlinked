use sea_orm_migration::{prelude::*, sea_orm::ConnectionTrait};

#[derive(DeriveMigrationName)]
pub struct Migration;

const CREATE_SEARCH_DOC_TABLE_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS hyperlink_search_doc (
        hyperlink_id INTEGER NOT NULL PRIMARY KEY,
        title TEXT NOT NULL,
        url TEXT NOT NULL,
        readable_text TEXT NOT NULL DEFAULT '',
        updated_at DATETIME NOT NULL,
        FOREIGN KEY (hyperlink_id) REFERENCES hyperlink(id) ON DELETE CASCADE
    )
"#;

const CREATE_SEARCH_FTS_TABLE_SQL: &str = r#"
    CREATE VIRTUAL TABLE IF NOT EXISTS hyperlink_search_fts USING fts5(
        title,
        url,
        readable_text,
        content='hyperlink_search_doc',
        content_rowid='hyperlink_id',
        tokenize='unicode61'
    )
"#;

const CREATE_DOC_INSERT_TRIGGER_SQL: &str = r#"
    CREATE TRIGGER IF NOT EXISTS trg_hyperlink_search_doc_ai
    AFTER INSERT ON hyperlink_search_doc
    BEGIN
        INSERT INTO hyperlink_search_fts (rowid, title, url, readable_text)
        VALUES (NEW.hyperlink_id, NEW.title, NEW.url, NEW.readable_text);
    END
"#;

const CREATE_DOC_DELETE_TRIGGER_SQL: &str = r#"
    CREATE TRIGGER IF NOT EXISTS trg_hyperlink_search_doc_ad
    AFTER DELETE ON hyperlink_search_doc
    BEGIN
        INSERT INTO hyperlink_search_fts (hyperlink_search_fts, rowid, title, url, readable_text)
        VALUES ('delete', OLD.hyperlink_id, OLD.title, OLD.url, OLD.readable_text);
    END
"#;

const CREATE_DOC_UPDATE_TRIGGER_SQL: &str = r#"
    CREATE TRIGGER IF NOT EXISTS trg_hyperlink_search_doc_au
    AFTER UPDATE ON hyperlink_search_doc
    BEGIN
        INSERT INTO hyperlink_search_fts (hyperlink_search_fts, rowid, title, url, readable_text)
        VALUES ('delete', OLD.hyperlink_id, OLD.title, OLD.url, OLD.readable_text);
        INSERT INTO hyperlink_search_fts (rowid, title, url, readable_text)
        VALUES (NEW.hyperlink_id, NEW.title, NEW.url, NEW.readable_text);
    END
"#;

const CREATE_HYPERLINK_INSERT_TRIGGER_SQL: &str = r#"
    CREATE TRIGGER IF NOT EXISTS trg_hyperlink_search_doc_hyperlink_ai
    AFTER INSERT ON hyperlink
    BEGIN
        INSERT INTO hyperlink_search_doc (hyperlink_id, title, url, readable_text, updated_at)
        VALUES (NEW.id, NEW.title, NEW.url, '', NEW.updated_at)
        ON CONFLICT(hyperlink_id) DO UPDATE SET
            title = excluded.title,
            url = excluded.url,
            updated_at = excluded.updated_at;
    END
"#;

const CREATE_HYPERLINK_UPDATE_TRIGGER_SQL: &str = r#"
    CREATE TRIGGER IF NOT EXISTS trg_hyperlink_search_doc_hyperlink_au
    AFTER UPDATE OF title, url, updated_at ON hyperlink
    BEGIN
        UPDATE hyperlink_search_doc
        SET title = NEW.title,
            url = NEW.url,
            updated_at = NEW.updated_at
        WHERE hyperlink_id = NEW.id;
    END
"#;

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

const BACKFILL_SEARCH_DOC_SQL: &str = r#"
    INSERT INTO hyperlink_search_doc (hyperlink_id, title, url, readable_text, updated_at)
    SELECT id, title, url, '', updated_at
    FROM hyperlink
    WHERE 1
    ON CONFLICT(hyperlink_id) DO UPDATE SET
        title = excluded.title,
        url = excluded.url,
        updated_at = excluded.updated_at
"#;

const BACKFILL_READABLE_TEXT_SQL: &str = r#"
    WITH ranked AS (
        SELECT
            hyperlink_id,
            CAST(payload AS TEXT) AS readable_text,
            ROW_NUMBER() OVER (
                PARTITION BY hyperlink_id
                ORDER BY created_at DESC, id DESC
            ) AS rn
        FROM hyperlink_artifact
        WHERE kind = 'readable_text'
    )
    UPDATE hyperlink_search_doc
    SET readable_text = (
            SELECT ranked.readable_text
            FROM ranked
            WHERE ranked.hyperlink_id = hyperlink_search_doc.hyperlink_id
              AND ranked.rn = 1
        ),
        updated_at = CURRENT_TIMESTAMP
    WHERE hyperlink_id IN (
        SELECT hyperlink_id
        FROM ranked
        WHERE rn = 1
    )
"#;

const DROP_READABLE_TEXT_INSERT_TRIGGER_SQL: &str =
    "DROP TRIGGER IF EXISTS trg_hyperlink_search_doc_readable_text_ai";
const DROP_HYPERLINK_UPDATE_TRIGGER_SQL: &str =
    "DROP TRIGGER IF EXISTS trg_hyperlink_search_doc_hyperlink_au";
const DROP_HYPERLINK_INSERT_TRIGGER_SQL: &str =
    "DROP TRIGGER IF EXISTS trg_hyperlink_search_doc_hyperlink_ai";
const DROP_DOC_UPDATE_TRIGGER_SQL: &str = "DROP TRIGGER IF EXISTS trg_hyperlink_search_doc_au";
const DROP_DOC_DELETE_TRIGGER_SQL: &str = "DROP TRIGGER IF EXISTS trg_hyperlink_search_doc_ad";
const DROP_DOC_INSERT_TRIGGER_SQL: &str = "DROP TRIGGER IF EXISTS trg_hyperlink_search_doc_ai";
const DROP_SEARCH_FTS_TABLE_SQL: &str = "DROP TABLE IF EXISTS hyperlink_search_fts";
const DROP_SEARCH_DOC_TABLE_SQL: &str = "DROP TABLE IF EXISTS hyperlink_search_doc";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();

        connection
            .execute_unprepared(CREATE_SEARCH_DOC_TABLE_SQL)
            .await?;
        connection
            .execute_unprepared(CREATE_SEARCH_FTS_TABLE_SQL)
            .await?;

        connection
            .execute_unprepared(CREATE_DOC_INSERT_TRIGGER_SQL)
            .await?;
        connection
            .execute_unprepared(CREATE_DOC_DELETE_TRIGGER_SQL)
            .await?;
        connection
            .execute_unprepared(CREATE_DOC_UPDATE_TRIGGER_SQL)
            .await?;
        connection
            .execute_unprepared(CREATE_HYPERLINK_INSERT_TRIGGER_SQL)
            .await?;
        connection
            .execute_unprepared(CREATE_HYPERLINK_UPDATE_TRIGGER_SQL)
            .await?;
        connection
            .execute_unprepared(CREATE_READABLE_TEXT_INSERT_TRIGGER_SQL)
            .await?;

        connection
            .execute_unprepared(BACKFILL_SEARCH_DOC_SQL)
            .await?;
        connection
            .execute_unprepared(BACKFILL_READABLE_TEXT_SQL)
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();

        connection
            .execute_unprepared(DROP_READABLE_TEXT_INSERT_TRIGGER_SQL)
            .await?;
        connection
            .execute_unprepared(DROP_HYPERLINK_UPDATE_TRIGGER_SQL)
            .await?;
        connection
            .execute_unprepared(DROP_HYPERLINK_INSERT_TRIGGER_SQL)
            .await?;
        connection
            .execute_unprepared(DROP_DOC_UPDATE_TRIGGER_SQL)
            .await?;
        connection
            .execute_unprepared(DROP_DOC_DELETE_TRIGGER_SQL)
            .await?;
        connection
            .execute_unprepared(DROP_DOC_INSERT_TRIGGER_SQL)
            .await?;
        connection
            .execute_unprepared(DROP_SEARCH_FTS_TABLE_SQL)
            .await?;
        connection
            .execute_unprepared(DROP_SEARCH_DOC_TABLE_SQL)
            .await?;

        Ok(())
    }
}
