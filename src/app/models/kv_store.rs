use std::collections::HashMap;

use sea_orm::{
    ActiveModelTrait,
    ActiveValue::Set,
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, Statement,
    entity::prelude::{DateTime, DateTimeUtc},
};

use crate::entity::app_kv;

const APP_KV_TABLE_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS app_kv (
        key TEXT NOT NULL PRIMARY KEY,
        value TEXT NOT NULL,
        updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
    )
"#;

pub async fn get(connection: &DatabaseConnection, key: &str) -> Result<Option<String>, DbErr> {
    ensure_table(connection).await?;
    Ok(app_kv::Entity::find_by_id(key.to_string())
        .one(connection)
        .await?
        .map(|row| row.value))
}

pub async fn set(connection: &DatabaseConnection, key: &str, value: &str) -> Result<(), DbErr> {
    ensure_table(connection).await?;
    let updated_at = now_utc();
    if let Some(existing) = app_kv::Entity::find_by_id(key.to_string())
        .one(connection)
        .await?
    {
        let mut active_model: app_kv::ActiveModel = existing.into();
        active_model.value = Set(value.to_string());
        active_model.updated_at = Set(updated_at);
        active_model.update(connection).await?;
    } else {
        app_kv::ActiveModel {
            key: Set(key.to_string()),
            value: Set(value.to_string()),
            updated_at: Set(updated_at),
        }
        .insert(connection)
        .await?;
    }
    Ok(())
}

pub async fn delete(connection: &DatabaseConnection, key: &str) -> Result<(), DbErr> {
    ensure_table(connection).await?;
    app_kv::Entity::delete_by_id(key.to_string())
        .exec(connection)
        .await
        .map(|_| ())
}

pub async fn get_many(
    connection: &DatabaseConnection,
    keys: &[&str],
) -> Result<HashMap<String, String>, DbErr> {
    ensure_table(connection).await?;
    if keys.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = app_kv::Entity::find()
        .filter(app_kv::Column::Key.is_in(keys.iter().map(|key| (*key).to_string())))
        .all(connection)
        .await?;

    let mut values = HashMap::with_capacity(rows.len());
    for row in rows {
        values.insert(row.key, row.value);
    }

    Ok(values)
}

async fn ensure_table(connection: &DatabaseConnection) -> Result<(), DbErr> {
    let backend = connection.get_database_backend();
    connection
        .execute_raw(Statement::from_string(
            backend,
            APP_KV_TABLE_SQL.to_string(),
        ))
        .await
        .map(|_| ())
}

fn now_utc() -> DateTime {
    DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}
#[cfg(test)]
#[path = "../../../tests/unit/app_models_kv_store.rs"]
mod tests;
