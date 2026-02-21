#[cfg(test)]
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};

#[cfg(test)]
const HYPERLINK_TABLE_SQL: &str = r#"
    CREATE TABLE hyperlink (
        id integer NOT NULL PRIMARY KEY AUTOINCREMENT,
        title varchar NOT NULL,
        url varchar NOT NULL,
        raw_url varchar NOT NULL DEFAULT '',
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
}

#[cfg(test)]
pub(crate) async fn execute_sql(connection: &DatabaseConnection, sql: &str) {
    connection
        .execute(Statement::from_string(
            connection.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("sql should execute");
}
