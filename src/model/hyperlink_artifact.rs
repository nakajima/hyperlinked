use std::{
    collections::HashMap,
    io::{Read, Write},
};

use flate2::{Compression, read::GzDecoder, write::GzEncoder};

use sea_orm::{
    ActiveModelTrait,
    ActiveValue::Set,
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    entity::prelude::{DateTime, DateTimeUtc},
};

use crate::entity::hyperlink_artifact::{self, HyperlinkArtifactKind};
use crate::storage::artifacts::{self, DISK_STORAGE_BACKEND};

pub const SNAPSHOT_WARC_CONTENT_TYPE: &str = "application/warc";
pub const SNAPSHOT_WARC_GZIP_CONTENT_TYPE: &str = "application/warc+gzip";
const GZIP_MAGIC_HEADER: [u8; 2] = [0x1f, 0x8b];

#[derive(Clone, Debug, Default)]
pub struct ArtifactBackfillReport {
    pub scanned: usize,
    pub migrated: usize,
    pub skipped_without_payload: usize,
}

#[derive(Clone, Debug, Default)]
pub struct SnapshotWarcCompressionBackfillReport {
    pub scanned: usize,
    pub compressed: usize,
    pub skipped_already_compressed: usize,
    pub failed: usize,
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

pub async fn load_processing_payload(
    artifact: &hyperlink_artifact::Model,
) -> Result<Vec<u8>, sea_orm::DbErr> {
    let payload = load_payload(artifact).await?;
    if artifact.kind != HyperlinkArtifactKind::SnapshotWarc {
        return Ok(payload);
    }
    if !is_gzip_payload(&payload) {
        return Ok(payload);
    }

    gzip_decode(&payload).map_err(|error| {
        sea_orm::DbErr::Custom(format!(
            "failed to decode gzip snapshot_warc payload for artifact {}: {error}",
            artifact.id
        ))
    })
}

pub fn compress_snapshot_warc_payload(payload: &[u8]) -> Result<Vec<u8>, sea_orm::DbErr> {
    gzip_encode(payload).map_err(|error| {
        sea_orm::DbErr::Custom(format!("failed to gzip snapshot_warc payload: {error}"))
    })
}

pub fn is_snapshot_warc_gzip_content_type(content_type: &str) -> bool {
    matches!(
        normalized_content_type(content_type).as_str(),
        SNAPSHOT_WARC_GZIP_CONTENT_TYPE | "application/gzip" | "application/x-gzip"
    )
}

pub fn is_snapshot_warc_gzip_artifact(artifact: &hyperlink_artifact::Model) -> bool {
    artifact.kind == HyperlinkArtifactKind::SnapshotWarc
        && is_snapshot_warc_gzip_content_type(&artifact.content_type)
}

pub fn is_gzip_payload(payload: &[u8]) -> bool {
    payload.starts_with(&GZIP_MAGIC_HEADER)
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

pub async fn backfill_snapshot_warc_payloads_to_gzip(
    connection: &DatabaseConnection,
    batch_size: u64,
) -> Result<SnapshotWarcCompressionBackfillReport, sea_orm::DbErr> {
    let mut report = SnapshotWarcCompressionBackfillReport::default();
    let mut last_id = 0i32;
    let batch_size = batch_size.clamp(1, 10_000);

    loop {
        let rows = hyperlink_artifact::Entity::find()
            .filter(hyperlink_artifact::Column::Id.gt(last_id))
            .filter(hyperlink_artifact::Column::Kind.eq(HyperlinkArtifactKind::SnapshotWarc))
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

            let artifact_id = row.id;
            match compress_snapshot_warc_row(connection, row).await {
                Ok(SnapshotWarcCompressionOutcome::Compressed) => report.compressed += 1,
                Ok(SnapshotWarcCompressionOutcome::SkippedAlreadyCompressed) => {
                    report.skipped_already_compressed += 1;
                }
                Err(error) => {
                    report.failed += 1;
                    tracing::warn!(
                        artifact_id,
                        error = %error,
                        "failed to backfill snapshot_warc artifact to gzip"
                    );
                }
            }
        }
    }

    Ok(report)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SnapshotWarcCompressionOutcome {
    Compressed,
    SkippedAlreadyCompressed,
}

async fn compress_snapshot_warc_row(
    connection: &DatabaseConnection,
    row: hyperlink_artifact::Model,
) -> Result<SnapshotWarcCompressionOutcome, String> {
    let artifact_id = row.id;
    let payload = load_payload(&row)
        .await
        .map_err(|error| format!("failed to load artifact payload: {error}"))?;

    if is_gzip_payload(&payload) {
        if !is_snapshot_warc_gzip_content_type(&row.content_type) {
            let size_bytes = i32::try_from(payload.len())
                .map_err(|_| "gzip payload size exceeded i32::MAX".to_string())?;
            let mut active: hyperlink_artifact::ActiveModel = row.into();
            active.content_type = Set(SNAPSHOT_WARC_GZIP_CONTENT_TYPE.to_string());
            active.size_bytes = Set(size_bytes);
            active
                .update(connection)
                .await
                .map_err(|error| format!("failed to normalize gzip artifact metadata: {error}"))?;
        }
        return Ok(SnapshotWarcCompressionOutcome::SkippedAlreadyCompressed);
    }

    let compressed = gzip_encode(&payload)
        .map_err(|error| format!("failed to gzip artifact payload: {error}"))?;
    let size_bytes = i32::try_from(compressed.len())
        .map_err(|_| "compressed payload size exceeded i32::MAX".to_string())?;
    let old_path = row.storage_path.clone();
    let stored = artifacts::write_payload(row.hyperlink_id, &row.kind, row.created_at, &compressed)
        .await
        .map_err(|error| format!("failed to write compressed payload to disk: {error}"))?;

    let mut active: hyperlink_artifact::ActiveModel = row.into();
    active.payload = Set(Vec::new());
    active.storage_path = Set(Some(stored.storage_path));
    active.storage_backend = Set(Some(DISK_STORAGE_BACKEND.to_string()));
    active.checksum_sha256 = Set(Some(stored.checksum_sha256));
    active.content_type = Set(SNAPSHOT_WARC_GZIP_CONTENT_TYPE.to_string());
    active.size_bytes = Set(size_bytes);
    active
        .update(connection)
        .await
        .map_err(|error| format!("failed to update compressed artifact metadata: {error}"))?;

    if let Some(old_path) = old_path.as_deref()
        && let Err(error) = artifacts::delete_if_exists(old_path).await
    {
        tracing::warn!(
            artifact_id,
            old_path,
            error = %error,
            "failed to delete legacy uncompressed snapshot payload after backfill"
        );
    }

    Ok(SnapshotWarcCompressionOutcome::Compressed)
}

fn normalized_content_type(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

fn gzip_encode(payload: &[u8]) -> Result<Vec<u8>, String> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(payload)
        .map_err(|error| format!("failed to write gzip payload: {error}"))?;
    encoder
        .finish()
        .map_err(|error| format!("failed to finish gzip stream: {error}"))
}

fn gzip_decode(payload: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoder = GzDecoder::new(payload);
    let mut decoded = Vec::new();
    decoder
        .read_to_end(&mut decoded)
        .map_err(|error| format!("failed to read gzip stream: {error}"))?;
    Ok(decoded)
}

fn now_utc() -> DateTime {
    DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact_with_payload(
        kind: HyperlinkArtifactKind,
        payload: Vec<u8>,
        content_type: &str,
    ) -> hyperlink_artifact::Model {
        hyperlink_artifact::Model {
            id: 1,
            hyperlink_id: 1,
            job_id: None,
            kind,
            payload,
            storage_path: None,
            storage_backend: None,
            checksum_sha256: None,
            content_type: content_type.to_string(),
            size_bytes: 0,
            created_at: now_utc(),
        }
    }

    #[test]
    fn gzip_compress_round_trips_snapshot_payload() {
        let raw = b"WARC/1.0\r\nWARC-Type: response\r\n\r\n<html>hello</html>";
        let compressed = gzip_encode(raw).expect("snapshot warc should gzip");
        assert!(is_gzip_payload(&compressed));
        let decoded = gzip_decode(&compressed).expect("gzip payload should decode");
        assert_eq!(decoded, raw);
    }

    #[tokio::test]
    async fn load_processing_payload_decodes_gzip_snapshot_warc() {
        let raw = b"WARC/1.0\r\nWARC-Type: response\r\n\r\n<html>hello</html>";
        let compressed = gzip_encode(raw).expect("snapshot warc should gzip");
        let artifact = artifact_with_payload(
            HyperlinkArtifactKind::SnapshotWarc,
            compressed,
            SNAPSHOT_WARC_GZIP_CONTENT_TYPE,
        );

        let payload = load_processing_payload(&artifact)
            .await
            .expect("processing payload should decode");
        assert_eq!(payload, raw);
    }

    #[tokio::test]
    async fn load_processing_payload_keeps_non_gzip_snapshot_warc_unchanged() {
        let raw = b"WARC/1.0\r\nWARC-Type: response\r\n\r\n<html>hello</html>".to_vec();
        let artifact = artifact_with_payload(
            HyperlinkArtifactKind::SnapshotWarc,
            raw.clone(),
            SNAPSHOT_WARC_CONTENT_TYPE,
        );

        let payload = load_processing_payload(&artifact)
            .await
            .expect("processing payload should load");
        assert_eq!(payload, raw);
    }
}
