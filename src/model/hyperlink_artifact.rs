use std::collections::HashMap;

use sea_orm::{
    ActiveModelTrait,
    ActiveValue::Set,
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    entity::prelude::{DateTime, DateTimeUtc},
};

use crate::entity::hyperlink_artifact::{self, HyperlinkArtifactKind};

pub async fn insert(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    job_id: Option<i32>,
    kind: HyperlinkArtifactKind,
    payload: Vec<u8>,
    content_type: &str,
) -> Result<hyperlink_artifact::Model, sea_orm::DbErr> {
    let size_bytes = i32::try_from(payload.len()).map_err(|_| {
        sea_orm::DbErr::Custom("artifact payload size exceeded i32::MAX".to_string())
    })?;

    hyperlink_artifact::ActiveModel {
        hyperlink_id: Set(hyperlink_id),
        job_id: Set(job_id),
        kind: Set(kind),
        payload: Set(payload),
        content_type: Set(content_type.to_string()),
        size_bytes: Set(size_bytes),
        created_at: Set(now_utc()),
        ..Default::default()
    }
    .insert(connection)
    .await
}

pub async fn latest_for_hyperlink_kind(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    kind: HyperlinkArtifactKind,
) -> Result<Option<hyperlink_artifact::Model>, sea_orm::DbErr> {
    hyperlink_artifact::Entity::find()
        .filter(hyperlink_artifact::Column::HyperlinkId.eq(hyperlink_id))
        .filter(hyperlink_artifact::Column::Kind.eq(kind))
        .order_by_desc(hyperlink_artifact::Column::CreatedAt)
        .order_by_desc(hyperlink_artifact::Column::Id)
        .one(connection)
        .await
}

pub async fn latest_for_hyperlink_kinds(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    kinds: &[HyperlinkArtifactKind],
) -> Result<HashMap<HyperlinkArtifactKind, hyperlink_artifact::Model>, sea_orm::DbErr> {
    if kinds.is_empty() {
        return Ok(HashMap::new());
    }

    let artifacts = hyperlink_artifact::Entity::find()
        .filter(hyperlink_artifact::Column::HyperlinkId.eq(hyperlink_id))
        .filter(hyperlink_artifact::Column::Kind.is_in(kinds.to_vec()))
        .order_by_desc(hyperlink_artifact::Column::CreatedAt)
        .order_by_desc(hyperlink_artifact::Column::Id)
        .all(connection)
        .await?;

    let mut latest = HashMap::with_capacity(kinds.len());
    for artifact in artifacts {
        latest.entry(artifact.kind.clone()).or_insert(artifact);
    }

    Ok(latest)
}

fn now_utc() -> DateTime {
    DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}
