use std::collections::HashMap;

use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, Statement, Value};

const APP_KV_TABLE_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS app_kv (
        key TEXT NOT NULL PRIMARY KEY,
        value TEXT NOT NULL,
        updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
    )
"#;

pub async fn get(connection: &DatabaseConnection, key: &str) -> Result<Option<String>, DbErr> {
    ensure_table(connection).await?;
    let backend = connection.get_database_backend();
    let row = connection
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT value FROM app_kv WHERE key = ?".to_string(),
            vec![Value::from(key.to_string())],
        ))
        .await?;

    Ok(row.and_then(|row| row.try_get::<String>("", "value").ok()))
}

pub async fn set(connection: &DatabaseConnection, key: &str, value: &str) -> Result<(), DbErr> {
    ensure_table(connection).await?;
    let backend = connection.get_database_backend();
    connection
        .execute(Statement::from_sql_and_values(
            backend,
            r#"
                INSERT INTO app_kv (key, value, updated_at)
                VALUES (?, ?, CURRENT_TIMESTAMP)
                ON CONFLICT(key) DO UPDATE SET
                    value = excluded.value,
                    updated_at = CURRENT_TIMESTAMP
            "#
            .to_string(),
            vec![Value::from(key.to_string()), Value::from(value.to_string())],
        ))
        .await
        .map(|_| ())
}

pub async fn delete(connection: &DatabaseConnection, key: &str) -> Result<(), DbErr> {
    ensure_table(connection).await?;
    let backend = connection.get_database_backend();
    connection
        .execute(Statement::from_sql_and_values(
            backend,
            "DELETE FROM app_kv WHERE key = ?".to_string(),
            vec![Value::from(key.to_string())],
        ))
        .await
        .map(|_| ())
}

pub async fn get_many(
    connection: &DatabaseConnection,
    keys: &[&str],
) -> Result<HashMap<String, String>, DbErr> {
    ensure_table(connection).await?;
    let mut values = HashMap::with_capacity(keys.len());
    for key in keys {
        if let Some(value) = get(connection, key).await? {
            values.insert((*key).to_string(), value);
        }
    }
    Ok(values)
}

async fn ensure_table(connection: &DatabaseConnection) -> Result<(), DbErr> {
    let backend = connection.get_database_backend();
    connection
        .execute(Statement::from_string(
            backend,
            APP_KV_TABLE_SQL.to_string(),
        ))
        .await
        .map(|_| ())
}
#[cfg(test)]
#[path = "../../../tests/unit/app_models_kv_store.rs"]
mod tests;
