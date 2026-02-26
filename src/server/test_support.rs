#[cfg(test)]
use sea_orm::{ConnectionTrait, Database, DatabaseConnection};

#[cfg(test)]
const HYPERLINK_TABLE_SQL: &str = r#"
    CREATE TABLE hyperlink (
        id integer NOT NULL PRIMARY KEY AUTOINCREMENT,
        title varchar NOT NULL,
        url varchar NOT NULL,
        raw_url varchar NOT NULL DEFAULT '',
        og_title text NULL,
        og_description text NULL,
        og_type text NULL,
        og_url text NULL,
        og_image_url text NULL,
        og_site_name text NULL,
        discovery_depth integer NOT NULL DEFAULT 0,
        clicks_count integer NOT NULL DEFAULT 0,
        last_clicked_at datetime_text NULL,
        created_at datetime_text NOT NULL,
        updated_at datetime_text NOT NULL
    );
"#;

#[cfg(test)]
const HYPERLINK_PROCESSING_JOB_TABLE_SQL: &str = r#"
    CREATE TABLE hyperlink_processing_job (
        id integer NOT NULL PRIMARY KEY AUTOINCREMENT,
        hyperlink_id integer NOT NULL,
        kind varchar NOT NULL DEFAULT 'snapshot',
        state varchar NOT NULL,
        error_message text NULL,
        queued_at datetime_text NOT NULL,
        started_at datetime_text NULL,
        finished_at datetime_text NULL,
        created_at datetime_text NOT NULL,
        updated_at datetime_text NOT NULL
    );
"#;

#[cfg(test)]
const HYPERLINK_ARTIFACT_TABLE_SQL: &str = r#"
    CREATE TABLE hyperlink_artifact (
        id integer NOT NULL PRIMARY KEY AUTOINCREMENT,
        hyperlink_id integer NOT NULL,
        job_id integer NULL,
        kind varchar NOT NULL,
        payload blob NOT NULL,
        storage_path text NULL,
        storage_backend text NULL,
        checksum_sha256 text NULL,
        content_type varchar NOT NULL,
        size_bytes integer NOT NULL,
        created_at datetime_text NOT NULL
    );
"#;

#[cfg(test)]
const HYPERLINK_RELATION_TABLE_SQL: &str = r#"
    CREATE TABLE hyperlink_relation (
        id integer NOT NULL PRIMARY KEY AUTOINCREMENT,
        parent_hyperlink_id integer NOT NULL,
        child_hyperlink_id integer NOT NULL,
        created_at datetime_text NOT NULL,
        UNIQUE(parent_hyperlink_id, child_hyperlink_id),
        CHECK(parent_hyperlink_id != child_hyperlink_id)
    );
"#;

#[cfg(test)]
const HYPERLINK_TOMBSTONE_TABLE_SQL: &str = r#"
    CREATE TABLE hyperlink_tombstone (
        hyperlink_id integer NOT NULL PRIMARY KEY,
        updated_at datetime_text NOT NULL
    );
"#;

#[cfg(test)]
const QUEUE_JOBS_TABLE_SQL: &str = r#"
    CREATE TABLE jobs (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        job_type TEXT NOT NULL,
        payload TEXT NOT NULL,
        status TEXT NOT NULL,
        attempts INTEGER NOT NULL DEFAULT 0,
        max_attempts INTEGER NOT NULL,
        available_at INTEGER NOT NULL,
        locked_at INTEGER NULL,
        lock_token TEXT NULL,
        last_error TEXT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        completed_at INTEGER NULL,
        first_enqueued_at INTEGER NULL,
        last_enqueued_at INTEGER NULL,
        first_started_at INTEGER NULL,
        last_started_at INTEGER NULL,
        last_finished_at INTEGER NULL,
        queued_ms_total INTEGER NOT NULL DEFAULT 0,
        queued_ms_last INTEGER NULL,
        processing_ms_total INTEGER NOT NULL DEFAULT 0,
        processing_ms_last INTEGER NULL
    );
"#;

#[cfg(test)]
const HYPERLINK_SEARCH_DOC_TABLE_SQL: &str = r#"
    CREATE TABLE hyperlink_search_doc (
        hyperlink_id integer NOT NULL PRIMARY KEY,
        title text NOT NULL,
        url text NOT NULL,
        readable_text text NOT NULL DEFAULT '',
        updated_at datetime_text NOT NULL,
        FOREIGN KEY(hyperlink_id) REFERENCES hyperlink(id) ON DELETE CASCADE
    );
"#;

#[cfg(test)]
const HYPERLINK_SEARCH_FTS_TABLE_SQL: &str = r#"
    CREATE VIRTUAL TABLE hyperlink_search_fts USING fts5(
        title,
        url,
        readable_text,
        content='hyperlink_search_doc',
        content_rowid='hyperlink_id',
        tokenize='unicode61'
    );
"#;

#[cfg(test)]
const HYPERLINK_SEARCH_DOC_INSERT_TRIGGER_SQL: &str = r#"
    CREATE TRIGGER trg_hyperlink_search_doc_ai
    AFTER INSERT ON hyperlink_search_doc
    BEGIN
        INSERT INTO hyperlink_search_fts (rowid, title, url, readable_text)
        VALUES (NEW.hyperlink_id, NEW.title, NEW.url, NEW.readable_text);
    END;
"#;

#[cfg(test)]
const HYPERLINK_SEARCH_DOC_DELETE_TRIGGER_SQL: &str = r#"
    CREATE TRIGGER trg_hyperlink_search_doc_ad
    AFTER DELETE ON hyperlink_search_doc
    BEGIN
        INSERT INTO hyperlink_search_fts (hyperlink_search_fts, rowid, title, url, readable_text)
        VALUES ('delete', OLD.hyperlink_id, OLD.title, OLD.url, OLD.readable_text);
    END;
"#;

#[cfg(test)]
const HYPERLINK_SEARCH_DOC_UPDATE_TRIGGER_SQL: &str = r#"
    CREATE TRIGGER trg_hyperlink_search_doc_au
    AFTER UPDATE ON hyperlink_search_doc
    BEGIN
        INSERT INTO hyperlink_search_fts (hyperlink_search_fts, rowid, title, url, readable_text)
        VALUES ('delete', OLD.hyperlink_id, OLD.title, OLD.url, OLD.readable_text);
        INSERT INTO hyperlink_search_fts (rowid, title, url, readable_text)
        VALUES (NEW.hyperlink_id, NEW.title, NEW.url, NEW.readable_text);
    END;
"#;

#[cfg(test)]
const HYPERLINK_SEARCH_DOC_HYPERLINK_INSERT_TRIGGER_SQL: &str = r#"
    CREATE TRIGGER trg_hyperlink_search_doc_hyperlink_ai
    AFTER INSERT ON hyperlink
    BEGIN
        INSERT INTO hyperlink_search_doc (hyperlink_id, title, url, readable_text, updated_at)
        VALUES (NEW.id, NEW.title, NEW.url, '', NEW.updated_at)
        ON CONFLICT(hyperlink_id) DO UPDATE SET
            title = excluded.title,
            url = excluded.url,
            updated_at = excluded.updated_at;
    END;
"#;

#[cfg(test)]
const HYPERLINK_SEARCH_DOC_HYPERLINK_UPDATE_TRIGGER_SQL: &str = r#"
    CREATE TRIGGER trg_hyperlink_search_doc_hyperlink_au
    AFTER UPDATE OF title, url, updated_at ON hyperlink
    BEGIN
        UPDATE hyperlink_search_doc
        SET title = NEW.title,
            url = NEW.url,
            updated_at = NEW.updated_at
        WHERE hyperlink_id = NEW.id;
    END;
"#;

#[cfg(test)]
const HYPERLINK_SEARCH_DOC_READABLE_TEXT_INSERT_TRIGGER_SQL: &str = r#"
    CREATE TRIGGER trg_hyperlink_search_doc_readable_text_ai
    AFTER INSERT ON hyperlink_artifact
    WHEN NEW.kind = 'readable_text'
    BEGIN
        INSERT INTO hyperlink_search_doc (hyperlink_id, title, url, readable_text, updated_at)
        SELECT h.id, h.title, h.url, CAST(NEW.payload AS text), CURRENT_TIMESTAMP
        FROM hyperlink h
        WHERE h.id = NEW.hyperlink_id
        ON CONFLICT(hyperlink_id) DO UPDATE SET
            title = excluded.title,
            url = excluded.url,
            readable_text = excluded.readable_text,
            updated_at = CURRENT_TIMESTAMP;
    END;
"#;

#[cfg(test)]
pub(crate) async fn new_memory_connection() -> DatabaseConnection {
    Database::connect("sqlite::memory:")
        .await
        .expect("in-memory database should initialize")
}

#[cfg(test)]
pub(crate) async fn initialize_jobs_schema(connection: &DatabaseConnection) {
    execute_sql(connection, HYPERLINK_TABLE_SQL).await;
    execute_sql(connection, HYPERLINK_PROCESSING_JOB_TABLE_SQL).await;
}

#[cfg(test)]
pub(crate) async fn initialize_hyperlinks_schema(connection: &DatabaseConnection) {
    initialize_jobs_schema(connection).await;
    execute_sql(connection, HYPERLINK_ARTIFACT_TABLE_SQL).await;
    execute_sql(connection, HYPERLINK_RELATION_TABLE_SQL).await;
    execute_sql(connection, HYPERLINK_TOMBSTONE_TABLE_SQL).await;
}

#[cfg(test)]
pub(crate) async fn initialize_hyperlinks_search_schema(connection: &DatabaseConnection) {
    execute_sql(connection, HYPERLINK_SEARCH_DOC_TABLE_SQL).await;
    execute_sql(connection, HYPERLINK_SEARCH_FTS_TABLE_SQL).await;
    execute_sql(connection, HYPERLINK_SEARCH_DOC_INSERT_TRIGGER_SQL).await;
    execute_sql(connection, HYPERLINK_SEARCH_DOC_DELETE_TRIGGER_SQL).await;
    execute_sql(connection, HYPERLINK_SEARCH_DOC_UPDATE_TRIGGER_SQL).await;
    execute_sql(
        connection,
        HYPERLINK_SEARCH_DOC_HYPERLINK_INSERT_TRIGGER_SQL,
    )
    .await;
    execute_sql(
        connection,
        HYPERLINK_SEARCH_DOC_HYPERLINK_UPDATE_TRIGGER_SQL,
    )
    .await;
    execute_sql(
        connection,
        HYPERLINK_SEARCH_DOC_READABLE_TEXT_INSERT_TRIGGER_SQL,
    )
    .await;
}

#[cfg(test)]
pub(crate) async fn initialize_hyperlinks_schema_with_search(connection: &DatabaseConnection) {
    initialize_hyperlinks_schema(connection).await;
    initialize_hyperlinks_search_schema(connection).await;
}

#[cfg(test)]
pub(crate) async fn initialize_queue_jobs_schema(connection: &DatabaseConnection) {
    execute_sql(connection, QUEUE_JOBS_TABLE_SQL).await;
}

#[cfg(test)]
pub(crate) async fn execute_sql(connection: &DatabaseConnection, sql: &str) {
    connection
        .execute_unprepared(sql)
        .await
        .expect("sql should execute");
}
