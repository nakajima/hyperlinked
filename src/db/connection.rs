use std::time::Duration;

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbErr, Statement};

pub fn database_url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| crate::db::MEMORY.to_string())
}

pub async fn init() -> Result<DatabaseConnection, DbErr> {
    let mut options = ConnectOptions::new(database_url());
    options
        .max_connections(env_u32("SERVER_DB_MAX_CONNECTIONS", 16, 1, 128))
        .min_connections(1)
        .connect_timeout(Duration::from_secs(10))
        .acquire_timeout(Duration::from_secs(10))
        .idle_timeout(Duration::from_secs(60))
        .max_lifetime(Duration::from_secs(60 * 60))
        .sqlx_logging(true);

    let connection = Database::connect(options).await?;
    apply_sqlite_pragmas(&connection).await?;
    Ok(connection)
}

fn env_u32(key: &str, default: u32, min: u32, max: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .map(|value| value.clamp(min, max))
        .unwrap_or(default.clamp(min, max))
}

async fn apply_sqlite_pragmas(connection: &DatabaseConnection) -> Result<(), DbErr> {
    let busy_timeout_ms = std::env::var("SQLITE_BUSY_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value.clamp(100, 60_000))
        .unwrap_or(5_000);

    let backend = connection.get_database_backend();
    let statements = [
        "PRAGMA journal_mode = WAL;",
        "PRAGMA synchronous = NORMAL;",
        "PRAGMA foreign_keys = ON;",
        &format!("PRAGMA busy_timeout = {busy_timeout_ms};"),
    ];

    for statement in statements {
        connection
            .execute(Statement::from_string(backend, statement.to_string()))
            .await?;
    }

    Ok(())
}
