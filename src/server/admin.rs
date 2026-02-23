use std::{
    collections::{HashMap, HashSet},
    io::{Cursor, Read, Write},
    path::{Component, Path},
};

use axum::{
    Router,
    extract::{Multipart, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing,
};
use sailfish::Template;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::FileOptions};

use crate::{
    entity::{
        hyperlink,
        hyperlink_artifact::{self, HyperlinkArtifactKind},
        hyperlink_processing_job::{self, HyperlinkProcessingJobKind, HyperlinkProcessingJobState},
        hyperlink_relation,
    },
    model::{
        hyperlink_artifact as hyperlink_artifact_model,
        hyperlink_processing_job::{self as hyperlink_processing_job_model, ProcessingQueueSender},
    },
    server::{
        context::Context,
        flash::{Flash, FlashName, redirect_with_flash},
    },
    storage::artifacts as artifact_storage,
};

use super::views;

const BACKUP_VERSION: u32 = 1;
const BACKUP_MANIFEST_PATH: &str = "manifest.json";
const BACKUP_HYPERLINKS_PATH: &str = "hyperlinks.json";
const BACKUP_RELATIONS_PATH: &str = "relations.json";
const BACKUP_ARTIFACTS_PATH: &str = "artifacts.json";
const BACKUP_ARTIFACTS_DIR: &str = "artifacts";

pub fn routes() -> Router<Context> {
    Router::new()
        .route("/admin", routing::get(index))
        .route("/admin/export", routing::get(export_hyperlinks))
        .route("/admin/import", routing::post(import_hyperlinks))
        .route(
            "/admin/process-missing-artifacts",
            routing::post(process_missing_artifacts),
        )
}

#[derive(Clone, Copy, Debug)]
struct LastRunSummary {
    snapshot_queued: usize,
    og_queued: usize,
    readability_queued: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct MissingArtifactsSummary {
    total_hyperlinks: usize,
    missing_source: usize,
    missing_og: usize,
    missing_readability: usize,
    snapshot_already_processing: usize,
    og_already_processing: usize,
    readability_already_processing: usize,
    snapshot_will_queue: usize,
    og_will_queue: usize,
    readability_will_queue: usize,
}

#[derive(Default)]
struct ArtifactPresence {
    has_source: bool,
    has_og_meta: bool,
    has_readable_text: bool,
    has_readable_meta: bool,
}

struct MissingArtifactsPlan {
    summary: MissingArtifactsSummary,
    snapshot_hyperlink_ids: Vec<i32>,
    og_hyperlink_ids: Vec<i32>,
    readability_hyperlink_ids: Vec<i32>,
}

#[derive(Clone, Copy, Debug, Default)]
struct AdminDatasetStats {
    total_hyperlinks: usize,
    root_hyperlinks: usize,
    discovered_hyperlinks: usize,
    total_artifacts: usize,
    total_processing_jobs: usize,
    active_processing_jobs: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct AdminImportSummary {
    hyperlinks: usize,
    relations: usize,
    artifacts: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BackupManifest {
    version: u32,
    exported_at: String,
    hyperlinks: usize,
    relations: usize,
    artifacts: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HyperlinkBackupRow {
    id: i32,
    title: String,
    url: String,
    raw_url: String,
    og_title: Option<String>,
    og_description: Option<String>,
    og_type: Option<String>,
    og_url: Option<String>,
    og_image_url: Option<String>,
    og_site_name: Option<String>,
    discovery_depth: i32,
    clicks_count: i32,
    last_clicked_at: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HyperlinkRelationBackupRow {
    id: i32,
    parent_hyperlink_id: i32,
    child_hyperlink_id: i32,
    created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HyperlinkArtifactBackupRow {
    id: i32,
    hyperlink_id: i32,
    kind: HyperlinkArtifactKind,
    content_type: String,
    size_bytes: i32,
    created_at: String,
    job_id: Option<i32>,
    checksum_sha256: Option<String>,
    payload_path: String,
}

#[derive(Clone, Debug)]
struct AdminBackupArchive {
    hyperlinks: Vec<HyperlinkBackupRow>,
    relations: Vec<HyperlinkRelationBackupRow>,
    artifacts: Vec<HyperlinkArtifactBackupRow>,
    artifact_payloads: HashMap<i32, Vec<u8>>,
}

async fn index(State(state): State<Context>, headers: HeaderMap) -> Response {
    let plan = match build_missing_artifacts_plan(&state.connection).await {
        Ok(plan) => plan,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load missing-artifact summary: {err}"),
            );
        }
    };

    let stats = match build_dataset_stats(&state.connection).await {
        Ok(stats) => stats,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load admin stats: {err}"),
            );
        }
    };

    views::render_html_page_with_flash(
        "Admin",
        render_index(&plan.summary, &stats),
        Flash::from_headers(&headers),
    )
}

async fn export_hyperlinks(State(state): State<Context>) -> Response {
    let payload = match build_backup_zip(&state.connection).await {
        Ok(payload) => payload,
        Err(message) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to build backup zip: {message}"),
            );
        }
    };

    (
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"hyperlinked-backup.zip\"".to_string(),
            ),
        ],
        payload,
    )
        .into_response()
}

async fn import_hyperlinks(
    State(state): State<Context>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let archive_bytes = match read_uploaded_backup_archive(&mut multipart).await {
        Ok(bytes) => bytes,
        Err(message) => {
            return redirect_with_flash(
                &headers,
                "/admin",
                FlashName::Alert,
                format!("Import failed: {message}"),
            );
        }
    };

    let summary = match import_backup_zip(&state.connection, archive_bytes).await {
        Ok(summary) => summary,
        Err(message) => {
            return redirect_with_flash(
                &headers,
                "/admin",
                FlashName::Alert,
                format!("Import failed: {message}"),
            );
        }
    };

    redirect_with_flash(
        &headers,
        "/admin",
        FlashName::Notice,
        format!(
            "Import complete: restored {} hyperlinks, {} relations, and {} artifacts.",
            summary.hyperlinks, summary.relations, summary.artifacts
        ),
    )
}

async fn process_missing_artifacts(State(state): State<Context>, headers: HeaderMap) -> Response {
    let plan = match build_missing_artifacts_plan(&state.connection).await {
        Ok(plan) => plan,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to compute missing-artifact plan: {err}"),
            );
        }
    };

    let result = match execute_missing_artifacts_plan(
        &state.connection,
        state.processing_queue.as_ref(),
        plan,
    )
    .await
    {
        Ok(result) => result,
        Err(err) => {
            return response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to enqueue missing-artifact jobs: {err}"),
            );
        }
    };

    redirect_with_flash(
        &headers,
        "/admin",
        FlashName::Notice,
        format!(
            "Queued {} snapshot job(s), {} og job(s), and {} readability job(s).",
            result.snapshot_queued, result.og_queued, result.readability_queued
        ),
    )
}

async fn build_backup_zip(connection: &DatabaseConnection) -> Result<Vec<u8>, String> {
    let hyperlinks = hyperlink::Entity::find()
        .order_by_asc(hyperlink::Column::Id)
        .all(connection)
        .await
        .map_err(|err| format!("failed to load hyperlinks: {err}"))?;
    let relations = hyperlink_relation::Entity::find()
        .order_by_asc(hyperlink_relation::Column::Id)
        .all(connection)
        .await
        .map_err(|err| format!("failed to load hyperlink relations: {err}"))?;
    let artifacts = hyperlink_artifact::Entity::find()
        .order_by_asc(hyperlink_artifact::Column::Id)
        .all(connection)
        .await
        .map_err(|err| format!("failed to load hyperlink artifacts: {err}"))?;

    let hyperlink_rows = hyperlinks
        .into_iter()
        .map(|model| HyperlinkBackupRow {
            id: model.id,
            title: model.title,
            url: model.url,
            raw_url: model.raw_url,
            og_title: model.og_title,
            og_description: model.og_description,
            og_type: model.og_type,
            og_url: model.og_url,
            og_image_url: model.og_image_url,
            og_site_name: model.og_site_name,
            discovery_depth: model.discovery_depth,
            clicks_count: model.clicks_count,
            last_clicked_at: model.last_clicked_at.as_ref().map(format_datetime),
            created_at: format_datetime(&model.created_at),
            updated_at: format_datetime(&model.updated_at),
        })
        .collect::<Vec<_>>();

    let relation_rows = relations
        .into_iter()
        .map(|model| HyperlinkRelationBackupRow {
            id: model.id,
            parent_hyperlink_id: model.parent_hyperlink_id,
            child_hyperlink_id: model.child_hyperlink_id,
            created_at: format_datetime(&model.created_at),
        })
        .collect::<Vec<_>>();

    let manifest = BackupManifest {
        version: BACKUP_VERSION,
        exported_at: format_datetime(&now_utc()),
        hyperlinks: hyperlink_rows.len(),
        relations: relation_rows.len(),
        artifacts: artifacts.len(),
    };

    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    write_zip_json_file(&mut writer, BACKUP_MANIFEST_PATH, &manifest)?;
    write_zip_json_file(&mut writer, BACKUP_HYPERLINKS_PATH, &hyperlink_rows)?;
    write_zip_json_file(&mut writer, BACKUP_RELATIONS_PATH, &relation_rows)?;

    let mut artifact_rows = Vec::with_capacity(artifacts.len());
    for artifact in artifacts {
        let payload = hyperlink_artifact_model::load_payload(&artifact)
            .await
            .map_err(|err| format!("failed to load payload for artifact {}: {err}", artifact.id))?;
        let payload_path = format!("{BACKUP_ARTIFACTS_DIR}/{}.bin", artifact.id);
        write_zip_binary_file(&mut writer, &payload_path, &payload)?;

        artifact_rows.push(HyperlinkArtifactBackupRow {
            id: artifact.id,
            hyperlink_id: artifact.hyperlink_id,
            kind: artifact.kind,
            content_type: artifact.content_type,
            size_bytes: artifact.size_bytes,
            created_at: format_datetime(&artifact.created_at),
            job_id: artifact.job_id,
            checksum_sha256: artifact.checksum_sha256,
            payload_path,
        });
    }
    write_zip_json_file(&mut writer, BACKUP_ARTIFACTS_PATH, &artifact_rows)?;

    let cursor = writer
        .finish()
        .map_err(|err| format!("failed to finalize zip archive: {err}"))?;
    Ok(cursor.into_inner())
}

fn write_zip_json_file<T: Serialize>(
    writer: &mut ZipWriter<Cursor<Vec<u8>>>,
    path: &str,
    value: &T,
) -> Result<(), String> {
    let payload = serde_json::to_vec_pretty(value)
        .map_err(|err| format!("failed to encode {path}: {err}"))?;
    write_zip_binary_file(writer, path, &payload)
}

fn write_zip_binary_file(
    writer: &mut ZipWriter<Cursor<Vec<u8>>>,
    path: &str,
    payload: &[u8],
) -> Result<(), String> {
    writer
        .start_file(
            path,
            FileOptions::default().compression_method(CompressionMethod::Deflated),
        )
        .map_err(|err| format!("failed to create {path} in zip archive: {err}"))?;
    writer
        .write_all(payload)
        .map_err(|err| format!("failed to write {path} in zip archive: {err}"))?;
    Ok(())
}

async fn read_uploaded_backup_archive(multipart: &mut Multipart) -> Result<Vec<u8>, String> {
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|err| format!("failed to read multipart form: {err}"))?
    {
        let field_name = field.name().map(ToString::to_string);
        if field_name.as_deref() != Some("archive") {
            continue;
        }
        let bytes = field
            .bytes()
            .await
            .map_err(|err| format!("failed to read uploaded zip file: {err}"))?;
        if bytes.is_empty() {
            return Err("uploaded backup ZIP file is empty".to_string());
        }
        return Ok(bytes.to_vec());
    }

    Err("no backup ZIP file uploaded (expected form field `archive`)".to_string())
}

async fn import_backup_zip(
    connection: &DatabaseConnection,
    archive_bytes: Vec<u8>,
) -> Result<AdminImportSummary, String> {
    let archive = parse_backup_zip(archive_bytes)?;
    restore_backup_archive(connection, archive).await
}

fn parse_backup_zip(archive_bytes: Vec<u8>) -> Result<AdminBackupArchive, String> {
    let mut archive = ZipArchive::new(Cursor::new(archive_bytes))
        .map_err(|err| format!("invalid zip archive: {err}"))?;

    let manifest: BackupManifest = read_zip_json_file(&mut archive, BACKUP_MANIFEST_PATH)?;
    if manifest.version != BACKUP_VERSION {
        return Err(format!(
            "unsupported backup version {}; expected {}",
            manifest.version, BACKUP_VERSION
        ));
    }

    let hyperlinks: Vec<HyperlinkBackupRow> =
        read_zip_json_file(&mut archive, BACKUP_HYPERLINKS_PATH)?;
    let relations: Vec<HyperlinkRelationBackupRow> =
        read_zip_json_file(&mut archive, BACKUP_RELATIONS_PATH)?;
    let artifacts: Vec<HyperlinkArtifactBackupRow> =
        read_zip_json_file(&mut archive, BACKUP_ARTIFACTS_PATH)?;

    if hyperlinks.len() != manifest.hyperlinks {
        return Err(format!(
            "hyperlinks count mismatch: manifest says {}, file contains {}",
            manifest.hyperlinks,
            hyperlinks.len()
        ));
    }
    if relations.len() != manifest.relations {
        return Err(format!(
            "relations count mismatch: manifest says {}, file contains {}",
            manifest.relations,
            relations.len()
        ));
    }
    if artifacts.len() != manifest.artifacts {
        return Err(format!(
            "artifacts count mismatch: manifest says {}, file contains {}",
            manifest.artifacts,
            artifacts.len()
        ));
    }

    let mut artifact_payloads = HashMap::with_capacity(artifacts.len());
    for artifact in &artifacts {
        validate_archive_entry_path(&artifact.payload_path)?;
        let payload = read_zip_binary_file(&mut archive, &artifact.payload_path)?;
        if artifact_payloads.insert(artifact.id, payload).is_some() {
            return Err(format!(
                "duplicate artifact id {} in artifacts.json",
                artifact.id
            ));
        }
    }

    Ok(AdminBackupArchive {
        hyperlinks,
        relations,
        artifacts,
        artifact_payloads,
    })
}

fn read_zip_json_file<T: DeserializeOwned>(
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
    path: &str,
) -> Result<T, String> {
    let mut entry = archive
        .by_name(path)
        .map_err(|err| format!("missing {path} in backup zip: {err}"))?;
    let mut content = String::new();
    entry
        .read_to_string(&mut content)
        .map_err(|err| format!("failed to read {path} from backup zip: {err}"))?;
    serde_json::from_str(&content)
        .map_err(|err| format!("failed to parse {path} from backup zip: {err}"))
}

fn read_zip_binary_file(
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
    path: &str,
) -> Result<Vec<u8>, String> {
    let mut entry = archive
        .by_name(path)
        .map_err(|err| format!("missing {path} in backup zip: {err}"))?;
    let mut payload = Vec::new();
    entry
        .read_to_end(&mut payload)
        .map_err(|err| format!("failed to read {path} from backup zip: {err}"))?;
    Ok(payload)
}

async fn restore_backup_archive(
    connection: &DatabaseConnection,
    archive: AdminBackupArchive,
) -> Result<AdminImportSummary, String> {
    let mut summary = AdminImportSummary::default();

    let mut hyperlinks = archive.hyperlinks;
    hyperlinks.sort_by_key(|row| row.id);
    for row in &hyperlinks {
        restore_hyperlink_row(connection, row).await?;
        summary.hyperlinks += 1;
    }

    let mut relations = archive.relations;
    relations.sort_by_key(|row| row.id);
    for row in &relations {
        restore_relation_row(connection, row).await?;
        summary.relations += 1;
    }

    let mut artifacts = archive.artifacts;
    artifacts.sort_by_key(|row| row.id);
    for row in &artifacts {
        let payload = archive
            .artifact_payloads
            .get(&row.id)
            .ok_or_else(|| format!("missing payload bytes for artifact {}", row.id))?;
        restore_artifact_row(connection, row, payload).await?;
        summary.artifacts += 1;
    }

    Ok(summary)
}

async fn restore_hyperlink_row(
    connection: &DatabaseConnection,
    row: &HyperlinkBackupRow,
) -> Result<(), String> {
    let created_at = parse_datetime(&row.created_at)
        .map_err(|err| format!("invalid created_at for hyperlink {}: {err}", row.id))?;
    let updated_at = parse_datetime(&row.updated_at)
        .map_err(|err| format!("invalid updated_at for hyperlink {}: {err}", row.id))?;
    let last_clicked_at = parse_optional_datetime(row.last_clicked_at.as_deref())
        .map_err(|err| format!("invalid last_clicked_at for hyperlink {}: {err}", row.id))?;

    if let Some(existing) = hyperlink::Entity::find_by_id(row.id)
        .one(connection)
        .await
        .map_err(|err| format!("failed to load hyperlink {}: {err}", row.id))?
    {
        let mut active: hyperlink::ActiveModel = existing.into();
        active.title = Set(row.title.clone());
        active.url = Set(row.url.clone());
        active.raw_url = Set(row.raw_url.clone());
        active.og_title = Set(row.og_title.clone());
        active.og_description = Set(row.og_description.clone());
        active.og_type = Set(row.og_type.clone());
        active.og_url = Set(row.og_url.clone());
        active.og_image_url = Set(row.og_image_url.clone());
        active.og_site_name = Set(row.og_site_name.clone());
        active.discovery_depth = Set(row.discovery_depth);
        active.clicks_count = Set(row.clicks_count);
        active.last_clicked_at = Set(last_clicked_at);
        active.created_at = Set(created_at);
        active.updated_at = Set(updated_at);
        active
            .update(connection)
            .await
            .map_err(|err| format!("failed to update hyperlink {}: {err}", row.id))?;
    } else {
        hyperlink::ActiveModel {
            id: Set(row.id),
            title: Set(row.title.clone()),
            url: Set(row.url.clone()),
            raw_url: Set(row.raw_url.clone()),
            og_title: Set(row.og_title.clone()),
            og_description: Set(row.og_description.clone()),
            og_type: Set(row.og_type.clone()),
            og_url: Set(row.og_url.clone()),
            og_image_url: Set(row.og_image_url.clone()),
            og_site_name: Set(row.og_site_name.clone()),
            discovery_depth: Set(row.discovery_depth),
            clicks_count: Set(row.clicks_count),
            last_clicked_at: Set(last_clicked_at),
            created_at: Set(created_at),
            updated_at: Set(updated_at),
            ..Default::default()
        }
        .insert(connection)
        .await
        .map_err(|err| format!("failed to insert hyperlink {}: {err}", row.id))?;
    }

    Ok(())
}

async fn restore_relation_row(
    connection: &DatabaseConnection,
    row: &HyperlinkRelationBackupRow,
) -> Result<(), String> {
    let created_at = parse_datetime(&row.created_at)
        .map_err(|err| format!("invalid created_at for relation {}: {err}", row.id))?;

    if let Some(existing) = hyperlink_relation::Entity::find_by_id(row.id)
        .one(connection)
        .await
        .map_err(|err| format!("failed to load relation {}: {err}", row.id))?
    {
        let mut active: hyperlink_relation::ActiveModel = existing.into();
        active.parent_hyperlink_id = Set(row.parent_hyperlink_id);
        active.child_hyperlink_id = Set(row.child_hyperlink_id);
        active.created_at = Set(created_at);
        active
            .update(connection)
            .await
            .map_err(|err| format!("failed to update relation {}: {err}", row.id))?;
        return Ok(());
    }

    let insert_result = hyperlink_relation::ActiveModel {
        id: Set(row.id),
        parent_hyperlink_id: Set(row.parent_hyperlink_id),
        child_hyperlink_id: Set(row.child_hyperlink_id),
        created_at: Set(created_at),
        ..Default::default()
    }
    .insert(connection)
    .await;

    if let Err(err) = insert_result {
        if let Some(existing) = hyperlink_relation::Entity::find()
            .filter(hyperlink_relation::Column::ParentHyperlinkId.eq(row.parent_hyperlink_id))
            .filter(hyperlink_relation::Column::ChildHyperlinkId.eq(row.child_hyperlink_id))
            .one(connection)
            .await
            .map_err(|load_err| {
                format!(
                    "failed to load existing relation ({}, {}): {load_err}",
                    row.parent_hyperlink_id, row.child_hyperlink_id
                )
            })?
        {
            let mut active: hyperlink_relation::ActiveModel = existing.into();
            active.created_at = Set(created_at);
            active.update(connection).await.map_err(|update_err| {
                format!(
                    "failed to update existing relation ({}, {}): {update_err}",
                    row.parent_hyperlink_id, row.child_hyperlink_id
                )
            })?;
            return Ok(());
        }
        return Err(format!("failed to insert relation {}: {err}", row.id));
    }

    Ok(())
}

async fn restore_artifact_row(
    connection: &DatabaseConnection,
    row: &HyperlinkArtifactBackupRow,
    payload: &[u8],
) -> Result<(), String> {
    let created_at = parse_datetime(&row.created_at)
        .map_err(|err| format!("invalid created_at for artifact {}: {err}", row.id))?;
    let size_bytes = i32::try_from(payload.len())
        .map_err(|_| format!("artifact {} payload exceeds i32::MAX bytes", row.id))?;
    let stored = artifact_storage::write_payload(row.hyperlink_id, &row.kind, created_at, payload)
        .await
        .map_err(|err| format!("failed to write payload for artifact {}: {err}", row.id))?;

    if let Some(existing) = hyperlink_artifact::Entity::find_by_id(row.id)
        .one(connection)
        .await
        .map_err(|err| format!("failed to load artifact {}: {err}", row.id))?
    {
        if let Some(old_path) = existing.storage_path.as_deref()
            && let Err(err) = artifact_storage::delete_if_exists(old_path).await
        {
            tracing::warn!(
                artifact_id = existing.id,
                storage_path = old_path,
                error = %err,
                "failed to delete existing artifact payload before restore"
            );
        }

        let mut active: hyperlink_artifact::ActiveModel = existing.into();
        active.hyperlink_id = Set(row.hyperlink_id);
        active.job_id = Set(None);
        active.kind = Set(row.kind.clone());
        active.payload = Set(Vec::new());
        active.storage_path = Set(Some(stored.storage_path));
        active.storage_backend = Set(Some(artifact_storage::DISK_STORAGE_BACKEND.to_string()));
        active.checksum_sha256 = Set(Some(stored.checksum_sha256));
        active.content_type = Set(row.content_type.clone());
        active.size_bytes = Set(size_bytes);
        active.created_at = Set(created_at);
        active
            .update(connection)
            .await
            .map_err(|err| format!("failed to update artifact {}: {err}", row.id))?;
    } else {
        hyperlink_artifact::ActiveModel {
            id: Set(row.id),
            hyperlink_id: Set(row.hyperlink_id),
            job_id: Set(None),
            kind: Set(row.kind.clone()),
            payload: Set(Vec::new()),
            storage_path: Set(Some(stored.storage_path)),
            storage_backend: Set(Some(artifact_storage::DISK_STORAGE_BACKEND.to_string())),
            checksum_sha256: Set(Some(stored.checksum_sha256)),
            content_type: Set(row.content_type.clone()),
            size_bytes: Set(size_bytes),
            created_at: Set(created_at),
        }
        .insert(connection)
        .await
        .map_err(|err| format!("failed to insert artifact {}: {err}", row.id))?;
    }

    Ok(())
}

fn validate_archive_entry_path(path: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("artifact payload path is empty".to_string());
    }

    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return Err(format!("artifact payload path must be relative: {path}"));
    }

    for component in candidate.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(format!("artifact payload path is unsafe: {path}"));
        }
    }

    Ok(())
}

fn now_utc() -> sea_orm::entity::prelude::DateTime {
    sea_orm::entity::prelude::DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}

fn format_datetime(value: &sea_orm::entity::prelude::DateTime) -> String {
    value.format("%Y-%m-%dT%H:%M:%S%.fZ").to_string()
}

fn parse_optional_datetime(
    value: Option<&str>,
) -> Result<Option<sea_orm::entity::prelude::DateTime>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    parse_datetime(value).map(Some)
}

fn parse_datetime(value: &str) -> Result<sea_orm::entity::prelude::DateTime, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("value is empty".to_string());
    }

    let value = value.strip_suffix('Z').unwrap_or(value);
    parse_naive_datetime(value).ok_or_else(|| format!("unsupported datetime format: {value}"))
}

fn parse_naive_datetime(value: &str) -> Option<sea_orm::entity::prelude::DateTime> {
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
    ] {
        if let Ok(parsed) = sea_orm::entity::prelude::DateTime::parse_from_str(value, format) {
            return Some(parsed);
        }
    }
    None
}

async fn build_missing_artifacts_plan(
    connection: &DatabaseConnection,
) -> Result<MissingArtifactsPlan, sea_orm::DbErr> {
    let hyperlink_ids = hyperlink::Entity::find()
        .select_only()
        .column(hyperlink::Column::Id)
        .into_tuple::<i32>()
        .all(connection)
        .await?;

    let mut summary = MissingArtifactsSummary {
        total_hyperlinks: hyperlink_ids.len(),
        ..Default::default()
    };

    if hyperlink_ids.is_empty() {
        return Ok(MissingArtifactsPlan {
            summary,
            snapshot_hyperlink_ids: Vec::new(),
            og_hyperlink_ids: Vec::new(),
            readability_hyperlink_ids: Vec::new(),
        });
    }

    let artifact_rows = hyperlink_artifact::Entity::find()
        .select_only()
        .column(hyperlink_artifact::Column::HyperlinkId)
        .column(hyperlink_artifact::Column::Kind)
        .filter(hyperlink_artifact::Column::HyperlinkId.is_in(hyperlink_ids.clone()))
        .filter(hyperlink_artifact::Column::Kind.is_in([
            HyperlinkArtifactKind::SnapshotWarc,
            HyperlinkArtifactKind::PdfSource,
            HyperlinkArtifactKind::OgMeta,
            HyperlinkArtifactKind::ReadableText,
            HyperlinkArtifactKind::ReadableMeta,
        ]))
        .into_tuple::<(i32, HyperlinkArtifactKind)>()
        .all(connection)
        .await?;

    let mut artifact_presence_by_hyperlink = HashMap::<i32, ArtifactPresence>::new();
    for (hyperlink_id, kind) in artifact_rows {
        let presence = artifact_presence_by_hyperlink
            .entry(hyperlink_id)
            .or_default();
        match kind {
            HyperlinkArtifactKind::SnapshotWarc | HyperlinkArtifactKind::PdfSource => {
                presence.has_source = true;
            }
            HyperlinkArtifactKind::OgMeta => {
                presence.has_og_meta = true;
            }
            HyperlinkArtifactKind::ReadableText => {
                presence.has_readable_text = true;
            }
            HyperlinkArtifactKind::ReadableMeta => {
                presence.has_readable_meta = true;
            }
            _ => {}
        }
    }

    let active_rows = hyperlink_processing_job::Entity::find()
        .select_only()
        .column(hyperlink_processing_job::Column::HyperlinkId)
        .column(hyperlink_processing_job::Column::Kind)
        .filter(hyperlink_processing_job::Column::HyperlinkId.is_in(hyperlink_ids.clone()))
        .filter(hyperlink_processing_job::Column::State.is_in([
            HyperlinkProcessingJobState::Queued,
            HyperlinkProcessingJobState::Running,
        ]))
        .filter(hyperlink_processing_job::Column::Kind.is_in([
            HyperlinkProcessingJobKind::Snapshot,
            HyperlinkProcessingJobKind::Og,
            HyperlinkProcessingJobKind::Readability,
        ]))
        .into_tuple::<(i32, HyperlinkProcessingJobKind)>()
        .all(connection)
        .await?;

    let mut snapshot_active_hyperlinks = HashSet::<i32>::new();
    let mut og_active_hyperlinks = HashSet::<i32>::new();
    let mut readability_active_hyperlinks = HashSet::<i32>::new();
    for (hyperlink_id, kind) in active_rows {
        match kind {
            HyperlinkProcessingJobKind::Snapshot => {
                snapshot_active_hyperlinks.insert(hyperlink_id);
            }
            HyperlinkProcessingJobKind::Og => {
                og_active_hyperlinks.insert(hyperlink_id);
            }
            HyperlinkProcessingJobKind::Readability => {
                readability_active_hyperlinks.insert(hyperlink_id);
            }
            _ => {}
        }
    }

    let mut snapshot_hyperlink_ids = Vec::new();
    let mut og_hyperlink_ids = Vec::new();
    let mut readability_hyperlink_ids = Vec::new();

    for hyperlink_id in hyperlink_ids {
        let presence = artifact_presence_by_hyperlink.get(&hyperlink_id);
        let has_source = presence.is_some_and(|presence| presence.has_source);
        if !has_source {
            summary.missing_source += 1;
            if snapshot_active_hyperlinks.contains(&hyperlink_id) {
                summary.snapshot_already_processing += 1;
            } else {
                snapshot_hyperlink_ids.push(hyperlink_id);
            }
            continue;
        }

        let has_og_meta = presence.is_some_and(|presence| presence.has_og_meta);
        if !has_og_meta {
            summary.missing_og += 1;
            if og_active_hyperlinks.contains(&hyperlink_id) {
                summary.og_already_processing += 1;
            } else {
                og_hyperlink_ids.push(hyperlink_id);
            }
        }

        let has_readable_artifacts = presence
            .is_some_and(|presence| presence.has_readable_text && presence.has_readable_meta);
        if !has_readable_artifacts {
            summary.missing_readability += 1;
            if readability_active_hyperlinks.contains(&hyperlink_id) {
                summary.readability_already_processing += 1;
            } else {
                readability_hyperlink_ids.push(hyperlink_id);
            }
        }
    }

    summary.snapshot_will_queue = snapshot_hyperlink_ids.len();
    summary.og_will_queue = og_hyperlink_ids.len();
    summary.readability_will_queue = readability_hyperlink_ids.len();

    Ok(MissingArtifactsPlan {
        summary,
        snapshot_hyperlink_ids,
        og_hyperlink_ids,
        readability_hyperlink_ids,
    })
}

async fn execute_missing_artifacts_plan(
    connection: &DatabaseConnection,
    queue: Option<&ProcessingQueueSender>,
    plan: MissingArtifactsPlan,
) -> Result<LastRunSummary, sea_orm::DbErr> {
    for hyperlink_id in &plan.snapshot_hyperlink_ids {
        hyperlink_processing_job_model::enqueue_for_hyperlink_kind(
            connection,
            *hyperlink_id,
            HyperlinkProcessingJobKind::Snapshot,
            queue,
        )
        .await?;
    }
    for hyperlink_id in &plan.og_hyperlink_ids {
        hyperlink_processing_job_model::enqueue_for_hyperlink_kind(
            connection,
            *hyperlink_id,
            HyperlinkProcessingJobKind::Og,
            queue,
        )
        .await?;
    }
    for hyperlink_id in &plan.readability_hyperlink_ids {
        hyperlink_processing_job_model::enqueue_for_hyperlink_kind(
            connection,
            *hyperlink_id,
            HyperlinkProcessingJobKind::Readability,
            queue,
        )
        .await?;
    }

    Ok(LastRunSummary {
        snapshot_queued: plan.snapshot_hyperlink_ids.len(),
        og_queued: plan.og_hyperlink_ids.len(),
        readability_queued: plan.readability_hyperlink_ids.len(),
    })
}

async fn build_dataset_stats(
    connection: &DatabaseConnection,
) -> Result<AdminDatasetStats, sea_orm::DbErr> {
    let total_hyperlinks = hyperlink::Entity::find().count(connection).await? as usize;
    let root_hyperlinks = hyperlink::Entity::find()
        .filter(hyperlink::Column::DiscoveryDepth.eq(0))
        .count(connection)
        .await? as usize;
    let discovered_hyperlinks = total_hyperlinks.saturating_sub(root_hyperlinks);

    let total_artifacts = hyperlink_artifact::Entity::find().count(connection).await? as usize;
    let total_processing_jobs = hyperlink_processing_job::Entity::find()
        .count(connection)
        .await? as usize;
    let active_processing_jobs = hyperlink_processing_job::Entity::find()
        .filter(hyperlink_processing_job::Column::State.is_in([
            HyperlinkProcessingJobState::Queued,
            HyperlinkProcessingJobState::Running,
        ]))
        .count(connection)
        .await? as usize;

    Ok(AdminDatasetStats {
        total_hyperlinks,
        root_hyperlinks,
        discovered_hyperlinks,
        total_artifacts,
        total_processing_jobs,
        active_processing_jobs,
    })
}

#[derive(Template)]
#[template(path = "admin/index.stpl")]
struct AdminIndexTemplate<'a> {
    summary: &'a MissingArtifactsSummary,
    stats: &'a AdminDatasetStats,
    has_missing_artifacts_to_process: bool,
}

fn render_index(
    summary: &MissingArtifactsSummary,
    stats: &AdminDatasetStats,
) -> Result<String, sailfish::RenderError> {
    AdminIndexTemplate {
        summary,
        stats,
        has_missing_artifacts_to_process: summary.snapshot_will_queue > 0
            || summary.og_will_queue > 0
            || summary.readability_will_queue > 0,
    }
    .render()
}

fn response_error(status: StatusCode, message: impl Into<String>) -> Response {
    views::render_error_page(status, message, "/admin", "Back to admin")
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum_test::multipart::{MultipartForm, Part};
    use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder};
    use std::io::Cursor;
    use zip::ZipArchive;

    use super::*;
    use crate::server::test_support;

    async fn new_server(seed_sql: &str) -> (axum_test::TestServer, sea_orm::DatabaseConnection) {
        let connection = test_support::new_memory_connection().await;
        test_support::initialize_hyperlinks_schema(&connection).await;
        test_support::execute_sql(&connection, seed_sql).await;

        let app = Router::<Context>::new()
            .merge(routes())
            .with_state(Context {
                connection: connection.clone(),
                processing_queue: None,
            });
        (
            axum_test::TestServer::new(app).expect("test server should initialize"),
            connection,
        )
    }

    #[tokio::test]
    async fn process_missing_artifacts_enqueues_snapshot_og_and_readability() {
        let (server, connection) = new_server(
            r#"
                INSERT INTO hyperlink (id, title, url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'No Artifacts', 'https://example.com/1', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00'),
                    (2, 'Source Only', 'https://example.com/2', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00'),
                    (3, 'Complete', 'https://example.com/3', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 2, NULL, 'snapshot_warc', X'77617263', 'application/warc', 4, '2026-02-21 00:01:00'),
                    (2, 3, NULL, 'snapshot_warc', X'77617263', 'application/warc', 4, '2026-02-21 00:01:00'),
                    (3, 3, NULL, 'og_meta', X'7B7D', 'application/json', 2, '2026-02-21 00:01:00'),
                    (4, 3, NULL, 'readable_text', X'74657874', 'text/markdown; charset=utf-8', 4, '2026-02-21 00:01:00'),
                    (5, 3, NULL, 'readable_meta', X'7B7D', 'application/json', 2, '2026-02-21 00:01:00');
            "#,
        )
        .await;

        let action = server.post("/admin/process-missing-artifacts").await;
        action.assert_status_see_other();
        action.assert_header("location", "/admin");

        let snapshot_jobs = hyperlink_processing_job::Entity::find()
            .filter(hyperlink_processing_job::Column::Kind.eq(HyperlinkProcessingJobKind::Snapshot))
            .count(&connection)
            .await
            .expect("snapshot jobs count should succeed");
        assert_eq!(snapshot_jobs, 1);

        let og_jobs = hyperlink_processing_job::Entity::find()
            .filter(hyperlink_processing_job::Column::Kind.eq(HyperlinkProcessingJobKind::Og))
            .count(&connection)
            .await
            .expect("og jobs count should succeed");
        assert_eq!(og_jobs, 1);

        let readability_jobs = hyperlink_processing_job::Entity::find()
            .filter(
                hyperlink_processing_job::Column::Kind.eq(HyperlinkProcessingJobKind::Readability),
            )
            .count(&connection)
            .await
            .expect("readability jobs count should succeed");
        assert_eq!(readability_jobs, 1);
    }

    #[tokio::test]
    async fn admin_page_shows_missing_artifact_summary() {
        let (server, _) = new_server(
            r#"
                INSERT INTO hyperlink (id, title, url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Missing Source', 'https://example.com/1', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00'),
                    (2, 'Missing Readability', 'https://example.com/2', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 2, NULL, 'snapshot_warc', X'77617263', 'application/warc', 4, '2026-02-21 00:01:00');
            "#,
        )
        .await;

        let page = server.get("/admin").await;
        page.assert_status_ok();
        let body = page.text();
        assert!(body.contains("Process all missing artifacts"));
        assert!(body.contains("Missing source"));
        assert!(body.contains("Missing Open Graph"));
        assert!(body.contains("Missing readability"));
        assert!(body.contains("Snapshot to queue"));
        assert!(body.contains("Open Graph to queue"));
        assert!(body.contains("Readability to queue"));
    }

    #[tokio::test]
    async fn process_missing_artifacts_skips_when_active_jobs_exist() {
        let (server, _) = new_server(
            r#"
                INSERT INTO hyperlink (id, title, url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Missing Source', 'https://example.com/1', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00'),
                    (2, 'Missing Readability', 'https://example.com/2', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 2, NULL, 'snapshot_warc', X'77617263', 'application/warc', 4, '2026-02-21 00:01:00');
                INSERT INTO hyperlink_processing_job (id, hyperlink_id, kind, state, error_message, queued_at, started_at, finished_at, created_at, updated_at)
                VALUES
                    (10, 1, 'snapshot', 'queued', NULL, '2026-02-21 00:02:00', NULL, NULL, '2026-02-21 00:02:00', '2026-02-21 00:02:00'),
                    (11, 2, 'readability', 'running', NULL, '2026-02-21 00:02:00', '2026-02-21 00:02:30', NULL, '2026-02-21 00:02:00', '2026-02-21 00:02:30');
            "#,
        )
        .await;

        let action = server.post("/admin/process-missing-artifacts").await;
        action.assert_status_see_other();
        action.assert_header("location", "/admin");
    }

    #[tokio::test]
    async fn admin_page_disables_process_button_when_no_work_is_needed() {
        let (server, _) = new_server(
            r#"
                INSERT INTO hyperlink (id, title, url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Complete', 'https://example.com/1', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 1, NULL, 'snapshot_warc', X'77617263', 'application/warc', 4, '2026-02-21 00:01:00'),
                    (2, 1, NULL, 'og_meta', X'7B7D', 'application/json', 2, '2026-02-21 00:01:00'),
                    (3, 1, NULL, 'readable_text', X'74657874', 'text/markdown; charset=utf-8', 4, '2026-02-21 00:01:00'),
                    (4, 1, NULL, 'readable_meta', X'7B7D', 'application/json', 2, '2026-02-21 00:01:00');
            "#,
        )
        .await;

        let page = server.get("/admin").await;
        page.assert_status_ok();
        let body = page.text();
        assert!(body.contains(
            "<input type=\"submit\" value=\"Process all missing artifacts\" disabled />"
        ));
    }

    #[tokio::test]
    async fn admin_page_shows_flash_style_examples() {
        let (server, _) = new_server(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Root', 'https://example.com/root', 'https://example.com/root', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00'),
                    (2, 'Child', 'https://example.com/child', 'https://example.com/child', 1, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (1, 1, NULL, 'snapshot_warc', X'77617263', 'application/warc', 4, '2026-02-21 00:01:00');
                INSERT INTO hyperlink_processing_job (id, hyperlink_id, kind, state, error_message, queued_at, started_at, finished_at, created_at, updated_at)
                VALUES
                    (1, 1, 'snapshot', 'queued', NULL, '2026-02-21 00:02:00', NULL, NULL, '2026-02-21 00:02:00', '2026-02-21 00:02:00'),
                    (2, 2, 'readability', 'succeeded', NULL, '2026-02-21 00:02:00', '2026-02-21 00:02:30', '2026-02-21 00:03:00', '2026-02-21 00:02:00', '2026-02-21 00:03:00');
            "#,
        )
        .await;

        let page = server.get("/admin").await;
        page.assert_status_ok();
        let body = page.text();
        assert!(body.contains("Diagnostics and examples"));
        assert!(body.contains("Dataset stats"));
        assert!(body.contains("Flash examples"));
        assert!(body.contains("border-notice-border"));
        assert!(body.contains("border-invalid"));
        assert!(body.contains("border-dev-alert-border"));
        assert!(body.contains("Root links"));
        assert!(body.contains("Discovered links"));
        assert!(body.contains("Active jobs"));
    }

    #[tokio::test]
    async fn admin_export_downloads_zip_payload_with_artifacts() {
        let (server, _) = new_server(
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Example export', 'https://example.com/canonical', 'https://example.com/raw?utm_source=test', 0, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00'),
                    (2, 'Discovered child', 'https://example.com/child', 'https://example.com/child', 1, 0, NULL, '2026-02-21 00:00:00', '2026-02-21 00:00:00');
                INSERT INTO hyperlink_relation (id, parent_hyperlink_id, child_hyperlink_id, created_at)
                VALUES
                    (3, 1, 2, '2026-02-21 00:00:30');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES
                    (9, 1, NULL, 'screenshot_png', X'89504E47', 'image/png', 4, '2026-02-21 00:01:00');
            "#,
        )
        .await;

        let export = server.get("/admin/export").await;
        export.assert_status_ok();
        export.assert_header("content-type", "application/zip");
        export.assert_header(
            "content-disposition",
            "attachment; filename=\"hyperlinked-backup.zip\"",
        );

        let mut archive = ZipArchive::new(Cursor::new(export.as_bytes().to_vec()))
            .expect("export should be a valid zip archive");
        let manifest: BackupManifest =
            read_zip_json_file(&mut archive, BACKUP_MANIFEST_PATH).expect("manifest should parse");
        assert_eq!(manifest.version, BACKUP_VERSION);
        assert_eq!(manifest.hyperlinks, 2);
        assert_eq!(manifest.relations, 1);
        assert_eq!(manifest.artifacts, 1);

        let hyperlinks: Vec<HyperlinkBackupRow> =
            read_zip_json_file(&mut archive, BACKUP_HYPERLINKS_PATH)
                .expect("hyperlinks should parse");
        assert_eq!(hyperlinks.len(), 2);
        assert_eq!(hyperlinks[0].title, "Example export");
        assert_eq!(
            hyperlinks[0].raw_url,
            "https://example.com/raw?utm_source=test"
        );

        let relations: Vec<HyperlinkRelationBackupRow> =
            read_zip_json_file(&mut archive, BACKUP_RELATIONS_PATH)
                .expect("relations should parse");
        assert_eq!(relations.len(), 1);
        assert_eq!(relations[0].parent_hyperlink_id, 1);
        assert_eq!(relations[0].child_hyperlink_id, 2);

        let artifacts: Vec<HyperlinkArtifactBackupRow> =
            read_zip_json_file(&mut archive, BACKUP_ARTIFACTS_PATH)
                .expect("artifacts should parse");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].id, 9);
        assert_eq!(artifacts[0].payload_path, "artifacts/9.bin");

        let payload = read_zip_binary_file(&mut archive, "artifacts/9.bin")
            .expect("artifact payload should exist");
        assert_eq!(payload, vec![0x89, 0x50, 0x4E, 0x47]);
    }

    #[tokio::test]
    async fn admin_import_restores_zip_payload_with_artifacts() {
        let (server, connection) = new_server("").await;

        let hyperlinks = vec![
            HyperlinkBackupRow {
                id: 11,
                title: "Imported One".to_string(),
                url: "https://example.com/one".to_string(),
                raw_url: "https://example.com/one?ref=raw".to_string(),
                og_title: Some("Imported OG".to_string()),
                og_description: None,
                og_type: None,
                og_url: None,
                og_image_url: None,
                og_site_name: None,
                discovery_depth: 0,
                clicks_count: 2,
                last_clicked_at: Some("2026-02-22T01:02:03Z".to_string()),
                created_at: "2026-02-22T00:00:00Z".to_string(),
                updated_at: "2026-02-22T00:00:00Z".to_string(),
            },
            HyperlinkBackupRow {
                id: 12,
                title: "Imported Two".to_string(),
                url: "https://example.com/two".to_string(),
                raw_url: "https://example.com/two".to_string(),
                og_title: None,
                og_description: None,
                og_type: None,
                og_url: None,
                og_image_url: None,
                og_site_name: None,
                discovery_depth: 1,
                clicks_count: 0,
                last_clicked_at: None,
                created_at: "2026-02-22T00:10:00Z".to_string(),
                updated_at: "2026-02-22T00:10:00Z".to_string(),
            },
        ];
        let relations = vec![HyperlinkRelationBackupRow {
            id: 20,
            parent_hyperlink_id: 11,
            child_hyperlink_id: 12,
            created_at: "2026-02-22T00:20:00Z".to_string(),
        }];
        let artifacts = vec![HyperlinkArtifactBackupRow {
            id: 33,
            hyperlink_id: 11,
            kind: HyperlinkArtifactKind::ScreenshotPng,
            content_type: "image/png".to_string(),
            size_bytes: 4,
            created_at: "2026-02-22T00:30:00Z".to_string(),
            job_id: None,
            checksum_sha256: None,
            payload_path: "artifacts/33.bin".to_string(),
        }];
        let manifest = BackupManifest {
            version: BACKUP_VERSION,
            exported_at: "2026-02-22T00:40:00Z".to_string(),
            hyperlinks: hyperlinks.len(),
            relations: relations.len(),
            artifacts: artifacts.len(),
        };

        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        write_zip_json_file(&mut writer, BACKUP_MANIFEST_PATH, &manifest)
            .expect("manifest should write");
        write_zip_json_file(&mut writer, BACKUP_HYPERLINKS_PATH, &hyperlinks)
            .expect("hyperlinks should write");
        write_zip_json_file(&mut writer, BACKUP_RELATIONS_PATH, &relations)
            .expect("relations should write");
        write_zip_binary_file(&mut writer, "artifacts/33.bin", &[0x89, 0x50, 0x4E, 0x47])
            .expect("artifact payload should write");
        write_zip_json_file(&mut writer, BACKUP_ARTIFACTS_PATH, &artifacts)
            .expect("artifacts should write");
        let archive_payload = writer
            .finish()
            .expect("zip writer should finish")
            .into_inner();

        let multipart = MultipartForm::new().add_part(
            "archive",
            Part::bytes(archive_payload)
                .file_name("backup.zip")
                .mime_type("application/zip"),
        );
        let import = server.post("/admin/import").multipart(multipart).await;
        import.assert_status_see_other();
        import.assert_header("location", "/admin");

        let count = hyperlink::Entity::find()
            .count(&connection)
            .await
            .expect("hyperlink count should succeed");
        assert_eq!(count, 2);

        let imported = hyperlink::Entity::find()
            .order_by_asc(hyperlink::Column::Id)
            .all(&connection)
            .await
            .expect("imported links should load");
        assert_eq!(imported[0].title, "Imported One");
        assert_eq!(imported[0].url, "https://example.com/one");
        assert_eq!(imported[0].raw_url, "https://example.com/one?ref=raw");
        assert_eq!(imported[0].og_title.as_deref(), Some("Imported OG"));
        assert_eq!(imported[1].title, "Imported Two");
        assert_eq!(imported[1].url, "https://example.com/two");

        let relation_count = hyperlink_relation::Entity::find()
            .count(&connection)
            .await
            .expect("relation count should succeed");
        assert_eq!(relation_count, 1);

        let artifact = hyperlink_artifact::Entity::find_by_id(33)
            .one(&connection)
            .await
            .expect("artifact lookup should succeed")
            .expect("artifact should exist");
        assert!(artifact.storage_path.is_some());
        assert!(artifact.payload.is_empty());
        let payload = hyperlink_artifact_model::load_payload(&artifact)
            .await
            .expect("artifact payload should load");
        assert_eq!(payload, vec![0x89, 0x50, 0x4E, 0x47]);
    }
}
