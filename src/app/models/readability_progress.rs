use std::collections::HashSet;

use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, entity::prelude::DateTime,
};

use super::kv_store;
use crate::entity::{app_kv, hyperlink};

const KEY_PREFIX: &str = "readability_progress.hyperlink.";

#[derive(Clone, Debug, PartialEq)]
pub struct ReadabilityProgress {
    pub hyperlink_id: i32,
    pub progress: f64,
    pub updated_at: DateTime,
}

pub async fn get(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
) -> Result<Option<ReadabilityProgress>, DbErr> {
    if !hyperlink_exists(connection, hyperlink_id).await? {
        kv_store::delete(connection, &key(hyperlink_id)).await?;
        return Ok(None);
    }

    let Some(entry) = kv_store::get_entry(connection, &key(hyperlink_id)).await? else {
        return Ok(None);
    };

    let progress = parse_entry(hyperlink_id, entry.value.as_str(), entry.updated_at);
    if progress.is_none() {
        kv_store::delete(connection, &key(hyperlink_id)).await?;
    }
    Ok(progress)
}

pub async fn set(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    progress: f64,
) -> Result<ReadabilityProgress, DbErr> {
    ensure_hyperlink_exists(connection, hyperlink_id).await?;
    let normalized_progress = progress.clamp(0.0, 1.0);
    let entry = kv_store::set_entry(
        connection,
        &key(hyperlink_id),
        &format!("{normalized_progress:.6}"),
    )
    .await?;

    Ok(ReadabilityProgress {
        hyperlink_id,
        progress: normalized_progress,
        updated_at: entry.updated_at,
    })
}

pub async fn restore(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    progress: f64,
    updated_at: DateTime,
) -> Result<ReadabilityProgress, DbErr> {
    ensure_hyperlink_exists(connection, hyperlink_id).await?;
    let normalized_progress = progress.clamp(0.0, 1.0);
    let entry = kv_store::set_entry_with_updated_at(
        connection,
        &key(hyperlink_id),
        &format!("{normalized_progress:.6}"),
        updated_at,
    )
    .await?;

    Ok(ReadabilityProgress {
        hyperlink_id,
        progress: normalized_progress,
        updated_at: entry.updated_at,
    })
}

pub async fn delete<C: ConnectionTrait>(connection: &C, hyperlink_id: i32) -> Result<(), DbErr> {
    kv_store::delete(connection, &key(hyperlink_id)).await
}

pub async fn list(connection: &DatabaseConnection) -> Result<Vec<ReadabilityProgress>, DbErr> {
    let rows = app_kv::Entity::find()
        .filter(app_kv::Column::Key.like(format!("{KEY_PREFIX}%")))
        .order_by_asc(app_kv::Column::Key)
        .all(connection)
        .await?;

    if rows.is_empty() {
        return Ok(Vec::new());
    }

    let existing_hyperlink_ids = hyperlink::Entity::find()
        .select_only()
        .column(hyperlink::Column::Id)
        .into_tuple::<i32>()
        .all(connection)
        .await?
        .into_iter()
        .collect::<HashSet<_>>();

    let mut progress_rows = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(hyperlink_id) = hyperlink_id_from_key(&row.key) else {
            continue;
        };
        if !existing_hyperlink_ids.contains(&hyperlink_id) {
            continue;
        }
        let Some(progress) = parse_entry(hyperlink_id, row.value.as_str(), row.updated_at) else {
            continue;
        };
        progress_rows.push(progress);
    }

    Ok(progress_rows)
}

fn key(hyperlink_id: i32) -> String {
    format!("{KEY_PREFIX}{hyperlink_id}")
}

fn hyperlink_id_from_key(key: &str) -> Option<i32> {
    key.strip_prefix(KEY_PREFIX)?.parse().ok()
}

async fn ensure_hyperlink_exists(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
) -> Result<(), DbErr> {
    if hyperlink_exists(connection, hyperlink_id).await? {
        return Ok(());
    }

    Err(DbErr::Custom(
        format!("hyperlink {hyperlink_id} not found").into(),
    ))
}

async fn hyperlink_exists(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
) -> Result<bool, DbErr> {
    Ok(hyperlink::Entity::find_by_id(hyperlink_id)
        .one(connection)
        .await?
        .is_some())
}

fn parse_entry(
    hyperlink_id: i32,
    value: &str,
    updated_at: DateTime,
) -> Option<ReadabilityProgress> {
    let parsed = value.trim().parse::<f64>().ok()?;
    if !parsed.is_finite() {
        return None;
    }

    Some(ReadabilityProgress {
        hyperlink_id,
        progress: parsed.clamp(0.0, 1.0),
        updated_at,
    })
}
