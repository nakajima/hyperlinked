use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, Schema};

use crate::entity::{
    app_kv, artifact_gc_pending, hyperlink, hyperlink_artifact, hyperlink_processing_job, hyperlink_relation, hyperlink_search_doc, hyperlink_tombstone, jobs, llm_interaction,
};

const MARK_DUPLICATE_ACTIVE_JOBS_FAILED_SQL: &str = r#"
    UPDATE hyperlink_processing_job
    SET
        state = 'failed',
        error_message = CASE
            WHEN error_message IS NULL OR TRIM(error_message) = ''
                THEN 'marked failed by schema sync: duplicate active job'
            ELSE error_message
        END,
        finished_at = COALESCE(finished_at, CURRENT_TIMESTAMP),
        updated_at = CURRENT_TIMESTAMP
    WHERE state IN ('queued', 'running')
      AND id NOT IN (
            SELECT MAX(id)
            FROM hyperlink_processing_job
            WHERE state IN ('queued', 'running')
            GROUP BY hyperlink_id, kind
      )
"#;

const CREATE_ACTIVE_JOB_UNIQUE_INDEX_SQL: &str = r#"
    CREATE UNIQUE INDEX IF NOT EXISTS idx_hyperlink_processing_job_active_unique
    ON hyperlink_processing_job (hyperlink_id, kind)
    WHERE state IN ('queued', 'running')
"#;

const DROP_LEGACY_ENTITY_UNIQUE_INDEXES_SQL: &[&str] = &[
    "DROP INDEX IF EXISTS idx_hyperlink_artifact_job_id_kind",
    "DROP INDEX IF EXISTS idx_hyperlink_relation_parent_child_unique",
];

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

const DROP_DOC_INSERT_TRIGGER_SQL: &str = "DROP TRIGGER IF EXISTS trg_hyperlink_search_doc_ai";
const DROP_DOC_DELETE_TRIGGER_SQL: &str = "DROP TRIGGER IF EXISTS trg_hyperlink_search_doc_ad";
const DROP_DOC_UPDATE_TRIGGER_SQL: &str = "DROP TRIGGER IF EXISTS trg_hyperlink_search_doc_au";
const DROP_HYPERLINK_INSERT_TRIGGER_SQL: &str =
    "DROP TRIGGER IF EXISTS trg_hyperlink_search_doc_hyperlink_ai";
const DROP_HYPERLINK_UPDATE_TRIGGER_SQL: &str =
    "DROP TRIGGER IF EXISTS trg_hyperlink_search_doc_hyperlink_au";
const DROP_READABLE_TEXT_INSERT_TRIGGER_SQL: &str =
    "DROP TRIGGER IF EXISTS trg_hyperlink_search_doc_readable_text_ai";

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

const CREATE_ARTIFACT_GC_PENDING_NEXT_ATTEMPT_INDEX_SQL: &str = r#"
    CREATE INDEX IF NOT EXISTS idx_artifact_gc_pending_next_attempt_at
    ON artifact_gc_pending (next_attempt_at, id)
"#;

const DROP_ARTIFACT_GC_TRIGGER_SQL: &str =
    "DROP TRIGGER IF EXISTS trg_artifact_gc_pending_hyperlink_artifact_ad";

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

pub async fn ensure_current(connection: &DatabaseConnection) -> Result<(), String> {
    sync(connection)
        .await
        .map_err(|err| format!("failed to synchronize database schema: {err}"))
}

pub async fn sync(connection: &DatabaseConnection) -> Result<(), DbErr> {
    drop_legacy_entity_unique_indexes(connection).await?;

    create_entity(connection, hyperlink::Entity).await?;
    create_entity(connection, hyperlink_processing_job::Entity).await?;
    create_entity(connection, hyperlink_artifact::Entity).await?;
    create_entity(connection, hyperlink_relation::Entity).await?;
    create_entity(connection, hyperlink_search_doc::Entity).await?;
    create_entity(connection, hyperlink_tombstone::Entity).await?;
    create_entity(connection, llm_interaction::Entity).await?;
    create_entity(connection, app_kv::Entity).await?;
    create_entity(connection, artifact_gc_pending::Entity).await?;
    create_entity(connection, jobs::Entity).await?;

    apply_sqlite_extras(connection).await
}

async fn drop_legacy_entity_unique_indexes(connection: &DatabaseConnection) -> Result<(), DbErr> {
    for statement in DROP_LEGACY_ENTITY_UNIQUE_INDEXES_SQL {
        connection.execute_unprepared(statement).await?;
    }

    Ok(())
}

async fn create_entity<E>(connection: &DatabaseConnection, entity: E) -> Result<(), DbErr>
where
    E: EntityTrait + Copy,
{
    let backend = connection.get_database_backend();
    let schema = Schema::new(backend);

    let mut create_table = schema.create_table_from_entity(entity);
    create_table.if_not_exists();
    connection.execute_raw(backend.build(&create_table)).await?;

    for mut create_index in schema.create_index_from_entity(entity) {
        create_index.if_not_exists();
        connection.execute_raw(backend.build(&create_index)).await?;
    }

    Ok(())
}

async fn apply_sqlite_extras(connection: &DatabaseConnection) -> Result<(), DbErr> {
    let statements = [
        MARK_DUPLICATE_ACTIVE_JOBS_FAILED_SQL,
        CREATE_ACTIVE_JOB_UNIQUE_INDEX_SQL,
        CREATE_SEARCH_FTS_TABLE_SQL,
        DROP_DOC_INSERT_TRIGGER_SQL,
        DROP_DOC_DELETE_TRIGGER_SQL,
        DROP_DOC_UPDATE_TRIGGER_SQL,
        DROP_HYPERLINK_INSERT_TRIGGER_SQL,
        DROP_HYPERLINK_UPDATE_TRIGGER_SQL,
        DROP_READABLE_TEXT_INSERT_TRIGGER_SQL,
        CREATE_DOC_INSERT_TRIGGER_SQL,
        CREATE_DOC_DELETE_TRIGGER_SQL,
        CREATE_DOC_UPDATE_TRIGGER_SQL,
        CREATE_HYPERLINK_INSERT_TRIGGER_SQL,
        CREATE_HYPERLINK_UPDATE_TRIGGER_SQL,
        CREATE_READABLE_TEXT_INSERT_TRIGGER_SQL,
        BACKFILL_SEARCH_DOC_SQL,
        BACKFILL_READABLE_TEXT_SQL,
        CREATE_ARTIFACT_GC_PENDING_NEXT_ATTEMPT_INDEX_SQL,
        DROP_ARTIFACT_GC_TRIGGER_SQL,
        CREATE_ARTIFACT_GC_TRIGGER_SQL,
    ];

    for statement in statements {
        connection.execute_unprepared(statement).await?;
    }

    Ok(())
}

#[cfg(test)]
#[path = "../../tests/unit/db_schema.rs"]
mod tests;
