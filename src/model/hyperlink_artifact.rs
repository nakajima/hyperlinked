use std::collections::HashMap;

use sea_orm::{
    ActiveModelTrait,
    ActiveValue::Set,
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    entity::prelude::{DateTime, DateTimeUtc},
};

use crate::entity::hyperlink_artifact::{self, HyperlinkArtifactKind};
use crate::storage::artifacts::{self, DISK_STORAGE_BACKEND};

#[derive(Clone, Debug, Default)]
pub struct ArtifactBackfillReport {
    pub scanned: usize,
    pub migrated: usize,
    pub skipped_without_payload: usize,
}

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
    let created_at = now_utc();
    let stored = artifacts::write_payload(hyperlink_id, &kind, created_at, &payload)
        .await
        .map_err(sea_orm::DbErr::Custom)?;

    hyperlink_artifact::ActiveModel {
        hyperlink_id: Set(hyperlink_id),
        job_id: Set(job_id),
        kind: Set(kind),
        payload: Set(Vec::new()),
        storage_path: Set(Some(stored.storage_path)),
        storage_backend: Set(Some(DISK_STORAGE_BACKEND.to_string())),
        checksum_sha256: Set(Some(stored.checksum_sha256)),
        content_type: Set(content_type.to_string()),
        size_bytes: Set(size_bytes),
        created_at: Set(created_at),
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

pub async fn latest_for_hyperlinks_kind(
    connection: &DatabaseConnection,
    hyperlink_ids: &[i32],
    kind: HyperlinkArtifactKind,
) -> Result<HashMap<i32, hyperlink_artifact::Model>, sea_orm::DbErr> {
    if hyperlink_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let artifacts = hyperlink_artifact::Entity::find()
        .filter(hyperlink_artifact::Column::HyperlinkId.is_in(hyperlink_ids.to_vec()))
        .filter(hyperlink_artifact::Column::Kind.eq(kind))
        .order_by_desc(hyperlink_artifact::Column::CreatedAt)
        .order_by_desc(hyperlink_artifact::Column::Id)
        .all(connection)
        .await?;

    let mut latest = HashMap::with_capacity(hyperlink_ids.len());
    for artifact in artifacts {
        latest.entry(artifact.hyperlink_id).or_insert(artifact);
    }

    Ok(latest)
}

pub async fn load_payload(artifact: &hyperlink_artifact::Model) -> Result<Vec<u8>, sea_orm::DbErr> {
    if let Some(storage_path) = artifact.storage_path.as_deref() {
        match artifacts::read_payload(storage_path).await {
            Ok(payload) => return Ok(payload),
            Err(error) if !artifact.payload.is_empty() => {
                tracing::warn!(
                    artifact_id = artifact.id,
                    hyperlink_id = artifact.hyperlink_id,
                    storage_path,
                    error = %error,
                    "falling back to legacy artifact payload from database"
                );
                return Ok(artifact.payload.clone());
            }
            Err(error) => return Err(sea_orm::DbErr::Custom(error)),
        }
    }

    if artifact.payload.is_empty() {
        return Err(sea_orm::DbErr::Custom(format!(
            "artifact {} has no stored payload",
            artifact.id
        )));
    }

    Ok(artifact.payload.clone())
}

pub async fn backfill_blob_payloads_to_disk(
    connection: &DatabaseConnection,
    batch_size: u64,
) -> Result<ArtifactBackfillReport, sea_orm::DbErr> {
    let mut report = ArtifactBackfillReport::default();
    let mut last_id = 0i32;
    let batch_size = batch_size.clamp(1, 10_000);

    loop {
        let rows = hyperlink_artifact::Entity::find()
            .filter(hyperlink_artifact::Column::Id.gt(last_id))
            .filter(hyperlink_artifact::Column::StoragePath.is_null())
            .order_by_asc(hyperlink_artifact::Column::Id)
            .limit(batch_size)
            .all(connection)
            .await?;

        if rows.is_empty() {
            break;
        }

        for row in rows {
            last_id = row.id;
            report.scanned += 1;

            if row.payload.is_empty() {
                report.skipped_without_payload += 1;
                continue;
            }

            let stored =
                artifacts::write_payload(row.hyperlink_id, &row.kind, row.created_at, &row.payload)
                    .await
                    .map_err(sea_orm::DbErr::Custom)?;

            let mut active: hyperlink_artifact::ActiveModel = row.into();
            active.payload = Set(Vec::new());
            active.storage_path = Set(Some(stored.storage_path));
            active.storage_backend = Set(Some(DISK_STORAGE_BACKEND.to_string()));
            active.checksum_sha256 = Set(Some(stored.checksum_sha256));
            active.update(connection).await?;
            report.migrated += 1;
        }
    }

    Ok(report)
}

fn now_utc() -> DateTime {
    DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}
