#![allow(dead_code)]

use sea_orm::{ConnectionTrait, Database, DatabaseConnection};

pub async fn new_memory_connection() -> DatabaseConnection {
    let connection = Database::connect(crate::db::MEMORY)
        .await
        .expect("in-memory database should initialize");

    for statement in [
        "PRAGMA journal_mode = WAL;",
        "PRAGMA synchronous = NORMAL;",
        "PRAGMA foreign_keys = ON;",
        "PRAGMA busy_timeout = 5000;",
    ] {
        connection
            .execute_unprepared(statement)
            .await
            .expect("sqlite pragmas should apply");
    }

    connection
}

pub async fn initialize_jobs_schema(connection: &DatabaseConnection) {
    crate::db::schema::sync(connection)
        .await
        .expect("schema sync should initialize jobs schema");
}

pub async fn initialize_hyperlinks_schema(connection: &DatabaseConnection) {
    crate::db::schema::sync(connection)
        .await
        .expect("schema sync should initialize hyperlinks schema");
}

pub async fn initialize_hyperlinks_search_schema(connection: &DatabaseConnection) {
    crate::db::schema::sync(connection)
        .await
        .expect("schema sync should initialize search schema");
}

pub async fn initialize_hyperlinks_schema_with_search(connection: &DatabaseConnection) {
    crate::db::schema::sync(connection)
        .await
        .expect("schema sync should initialize hyperlinks schema");
}

pub async fn initialize_queue_jobs_schema(connection: &DatabaseConnection) {
    crate::db::schema::sync(connection)
        .await
        .expect("queue schema should initialize");
}

pub async fn execute_sql(connection: &DatabaseConnection, sql: &str) {
    connection
        .execute_unprepared(sql)
        .await
        .expect("sql should execute");
}
