use std::{
    collections::{HashMap, HashSet},
    io::{Read, Seek, Write},
    path::{Component, Path, PathBuf},
};

use axum::extract::Multipart;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::AsyncWriteExt;
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::FileOptions};

use crate::{
    app::models::{hyperlink_artifact as hyperlink_artifact_model, readability_progress},
    entity::{
        hyperlink,
        hyperlink_artifact::{self, HyperlinkArtifactKind},
        hyperlink_relation,
    },
    server::{
        admin_backup::{BackupCompletionSummary, BackupProgress, BackupProgressStage},
        admin_import::{
            ImportCompletionSummary, ImportProgress, ImportProgressStage, next_import_upload_path,
        },
    },
    storage::artifacts as artifact_storage,
};

pub(crate) const BACKUP_VERSION: u32 = 1;
pub(crate) const BACKUP_MANIFEST_PATH: &str = "manifest.json";
pub(crate) const BACKUP_HYPERLINKS_PATH: &str = "hyperlinks.json";
pub(crate) const BACKUP_RELATIONS_PATH: &str = "relations.json";
pub(crate) const BACKUP_ARTIFACTS_PATH: &str = "artifacts.json";
pub(crate) const BACKUP_READABILITY_PROGRESS_PATH: &str = "readability_progress.json";
const BACKUP_ARTIFACTS_DIR: &str = "artifacts";
const BACKUP_ARTIFACT_READ_CONCURRENCY: usize = 4;
const BACKUP_DEFLATE_LEVEL_BEST: i32 = 9;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct BackupManifest {
    pub(crate) version: u32,
    pub(crate) exported_at: String,
    pub(crate) hyperlinks: usize,
    pub(crate) relations: usize,
    pub(crate) artifacts: usize,
    #[serde(default)]
    pub(crate) readability_progress: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct HyperlinkBackupRow {
    pub(crate) id: i32,
    pub(crate) title: String,
    pub(crate) url: String,
    pub(crate) raw_url: String,
    pub(crate) summary: Option<String>,
    pub(crate) og_title: Option<String>,
    pub(crate) og_description: Option<String>,
    pub(crate) og_type: Option<String>,
    pub(crate) og_url: Option<String>,
    pub(crate) og_image_url: Option<String>,
    pub(crate) og_site_name: Option<String>,
    pub(crate) discovery_depth: i32,
    pub(crate) clicks_count: i32,
    pub(crate) last_clicked_at: Option<String>,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct HyperlinkRelationBackupRow {
    pub(crate) id: i32,
    pub(crate) parent_hyperlink_id: i32,
    pub(crate) child_hyperlink_id: i32,
    pub(crate) created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct HyperlinkArtifactBackupRow {
    pub(crate) id: i32,
    pub(crate) hyperlink_id: i32,
    pub(crate) kind: HyperlinkArtifactKind,
    pub(crate) content_type: String,
    pub(crate) size_bytes: i32,
    pub(crate) created_at: String,
    pub(crate) job_id: Option<i32>,
    pub(crate) checksum_sha256: Option<String>,
    pub(crate) payload_path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ReadabilityProgressBackupRow {
    pub(crate) hyperlink_id: i32,
    pub(crate) progress: f64,
    pub(crate) updated_at: String,
}

#[derive(Clone, Debug)]
struct AdminBackupArchive {
    hyperlinks: Vec<HyperlinkBackupRow>,
    relations: Vec<HyperlinkRelationBackupRow>,
    artifacts: Vec<HyperlinkArtifactBackupRow>,
    readability_progress: Vec<ReadabilityProgressBackupRow>,
}

pub(super) async fn build_backup_zip<F>(
    connection: &DatabaseConnection,
    output_path: &Path,
    mut report_progress: F,
) -> Result<BackupCompletionSummary, String>
where
    F: FnMut(BackupProgress),
{
    let build_result = async {
        report_progress(BackupProgress {
            stage: BackupProgressStage::LoadingRecords,
            artifacts_done: 0,
            artifacts_total: 0,
        });

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
        let readability_progress_rows = readability_progress::list(connection)
            .await
            .map_err(|err| format!("failed to load readability progress: {err}"))?
            .into_iter()
            .map(|row| ReadabilityProgressBackupRow {
                hyperlink_id: row.hyperlink_id,
                progress: row.progress,
                updated_at: format_datetime(&row.updated_at),
            })
            .collect::<Vec<_>>();

        let hyperlink_rows = hyperlinks
            .into_iter()
            .map(|model| HyperlinkBackupRow {
                id: model.id,
                title: model.title,
                url: model.url,
                raw_url: model.raw_url,
                summary: model.summary,
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
            readability_progress: readability_progress_rows.len(),
        };

        let output_file = std::fs::File::create(output_path)
            .map_err(|err| format!("failed to create backup output file: {err}"))?;
        let mut writer = ZipWriter::new(output_file);
        write_zip_json_file(&mut writer, BACKUP_MANIFEST_PATH, &manifest)?;
        write_zip_json_file(&mut writer, BACKUP_HYPERLINKS_PATH, &hyperlink_rows)?;
        write_zip_json_file(&mut writer, BACKUP_RELATIONS_PATH, &relation_rows)?;
        write_zip_json_file(
            &mut writer,
            BACKUP_READABILITY_PROGRESS_PATH,
            &readability_progress_rows,
        )?;

        let artifacts_total = artifacts.len();
        report_progress(BackupProgress {
            stage: BackupProgressStage::PackingArtifacts,
            artifacts_done: 0,
            artifacts_total,
        });

        let mut artifact_rows = Vec::with_capacity(artifacts_total);
        let mut payload_tasks = tokio::task::JoinSet::new();
        let mut next_submit_index = 0usize;
        let mut next_write_index = 0usize;
        let mut ready_payloads = HashMap::<usize, (hyperlink_artifact::Model, Vec<u8>)>::new();

        while next_submit_index < artifacts_total
            && payload_tasks.len() < BACKUP_ARTIFACT_READ_CONCURRENCY
        {
            let task_index = next_submit_index;
            let artifact = artifacts[task_index].clone();
            payload_tasks.spawn(async move {
                let artifact_id = artifact.id;
                let payload = hyperlink_artifact_model::load_payload(&artifact)
                    .await
                    .map_err(|err| {
                        format!("failed to load payload for artifact {artifact_id}: {err}")
                    })?;
                Ok::<_, String>((task_index, artifact, payload))
            });
            next_submit_index += 1;
        }

        while let Some(joined) = payload_tasks.join_next().await {
            let (task_index, artifact, payload) =
                joined.map_err(|err| format!("artifact payload task failed: {err}"))??;
            ready_payloads.insert(task_index, (artifact, payload));

            while let Some((artifact, payload)) = ready_payloads.remove(&next_write_index) {
                let payload_path = format!("{BACKUP_ARTIFACTS_DIR}/{}.bin", artifact.id);
                write_zip_binary_file_with_compression(
                    &mut writer,
                    &payload_path,
                    &payload,
                    CompressionMethod::Deflated,
                )?;

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

                next_write_index += 1;
                report_progress(BackupProgress {
                    stage: BackupProgressStage::PackingArtifacts,
                    artifacts_done: artifact_rows.len(),
                    artifacts_total,
                });
            }

            while next_submit_index < artifacts_total
                && payload_tasks.len() < BACKUP_ARTIFACT_READ_CONCURRENCY
            {
                let task_index = next_submit_index;
                let artifact = artifacts[task_index].clone();
                payload_tasks.spawn(async move {
                    let artifact_id = artifact.id;
                    let payload = hyperlink_artifact_model::load_payload(&artifact)
                        .await
                        .map_err(|err| {
                            format!("failed to load payload for artifact {artifact_id}: {err}")
                        })?;
                    Ok::<_, String>((task_index, artifact, payload))
                });
                next_submit_index += 1;
            }
        }
        write_zip_json_file(&mut writer, BACKUP_ARTIFACTS_PATH, &artifact_rows)?;

        report_progress(BackupProgress {
            stage: BackupProgressStage::Finalizing,
            artifacts_done: artifact_rows.len(),
            artifacts_total,
        });

        writer
            .finish()
            .map_err(|err| format!("failed to finalize zip archive: {err}"))?;

        Ok(BackupCompletionSummary {
            hyperlinks: hyperlink_rows.len(),
            relations: relation_rows.len(),
            artifacts: artifact_rows.len(),
        })
    }
    .await;

    if build_result.is_err() {
        if let Err(error) = tokio::fs::remove_file(output_path).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %output_path.display(),
                error = %error,
                "failed to clean up incomplete backup archive"
            );
        }
    }

    build_result
}

pub(crate) fn write_zip_json_file<T: Serialize, W: Write + Seek>(
    writer: &mut ZipWriter<W>,
    path: &str,
    value: &T,
) -> Result<(), String> {
    let payload = serde_json::to_vec_pretty(value)
        .map_err(|err| format!("failed to encode {path}: {err}"))?;
    write_zip_binary_file_with_compression(writer, path, &payload, CompressionMethod::Deflated)
}

pub(crate) fn write_zip_binary_file_with_compression<W: Write + Seek>(
    writer: &mut ZipWriter<W>,
    path: &str,
    payload: &[u8],
    compression: CompressionMethod,
) -> Result<(), String> {
    let mut file_options = FileOptions::default().compression_method(compression);
    if compression == CompressionMethod::Deflated {
        file_options = file_options.compression_level(Some(BACKUP_DEFLATE_LEVEL_BEST));
    }
    writer
        .start_file(path, file_options)
        .map_err(|err| format!("failed to create {path} in zip archive: {err}"))?;
    writer
        .write_all(payload)
        .map_err(|err| format!("failed to write {path} in zip archive: {err}"))?;
    Ok(())
}

pub(super) async fn read_uploaded_backup_archive(
    multipart: &mut Multipart,
) -> Result<PathBuf, String> {
    let mut seen_field_names: Vec<String> = Vec::new();

    while let Some(mut field) = multipart.next_field().await.map_err(|err| {
        tracing::warn!(error = %err, "failed to read multipart form for admin import");
        format!("failed to read multipart form: {err}")
    })? {
        let field_name = field.name().map(ToString::to_string);
        let file_name = field.file_name().map(ToString::to_string);
        let field_content_type = field.content_type().map(ToString::to_string);
        let field_name_label = field_name.as_deref().unwrap_or("<none>");

        seen_field_names.push(field_name_label.to_string());
        tracing::info!(
            field_name = %field_name_label,
            file_name = %file_name.as_deref().unwrap_or(""),
            field_content_type = %field_content_type.as_deref().unwrap_or(""),
            "received multipart field for admin import"
        );

        if field_name.as_deref() != Some("archive") {
            tracing::info!(
                field_name = %field_name_label,
                "ignoring multipart field for admin import because it is not `archive`"
            );
            continue;
        }

        let upload_path = next_import_upload_path();
        let mut file = tokio::fs::File::create(&upload_path)
            .await
            .map_err(|err| format!("failed to open temporary upload file: {err}"))?;

        let mut bytes_written: u64 = 0;
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|err| format!("failed to read uploaded zip file chunk: {err}"))?
        {
            file.write_all(&chunk)
                .await
                .map_err(|err| format!("failed to write uploaded zip file chunk: {err}"))?;
            bytes_written = bytes_written.saturating_add(chunk.len() as u64);
        }

        if let Err(err) = file.flush().await {
            remove_uploaded_backup_file(&upload_path).await;
            return Err(format!("failed to finalize uploaded zip file: {err}"));
        }

        if bytes_written == 0 {
            tracing::warn!(
                field_name = %field_name_label,
                file_name = %file_name.as_deref().unwrap_or(""),
                "uploaded admin import archive file is empty"
            );
            remove_uploaded_backup_file(&upload_path).await;
            return Err("uploaded backup ZIP file is empty".to_string());
        }

        tracing::info!(
            field_name = %field_name_label,
            file_name = %file_name.as_deref().unwrap_or(""),
            bytes_written,
            "accepted backup archive upload for admin import"
        );
        return Ok(upload_path);
    }

    tracing::warn!(
        seen_field_names = %seen_field_names.join(","),
        hint = "multipart body did not include an `archive` file part; ensure the upload input is enabled when submitting",
        "admin import upload did not include required multipart field `archive`"
    );
    Err("no backup ZIP file uploaded (expected form field `archive`)".to_string())
}

async fn remove_uploaded_backup_file(path: &Path) {
    if let Err(error) = tokio::fs::remove_file(path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(
            path = %path.display(),
            error = %error,
            "failed to remove uploaded backup file"
        );
    }
}

pub(super) async fn import_backup_zip<F>(
    connection: &DatabaseConnection,
    archive_path: &Path,
    mut report_progress: F,
) -> Result<ImportCompletionSummary, String>
where
    F: FnMut(ImportProgress),
{
    let file = std::fs::File::open(archive_path)
        .map_err(|err| format!("failed to open uploaded zip archive: {err}"))?;
    let mut zip_archive =
        ZipArchive::new(file).map_err(|err| format!("invalid zip archive: {err}"))?;

    report_progress(ImportProgress {
        stage: ImportProgressStage::Validating,
        hyperlinks_done: 0,
        hyperlinks_total: 0,
        relations_done: 0,
        relations_total: 0,
        artifacts_done: 0,
        artifacts_total: 0,
    });

    let manifest: BackupManifest = read_zip_json_file(&mut zip_archive, BACKUP_MANIFEST_PATH)?;
    if manifest.version != BACKUP_VERSION {
        return Err(format!(
            "unsupported backup version {}; expected {}",
            manifest.version, BACKUP_VERSION
        ));
    }

    let hyperlinks: Vec<HyperlinkBackupRow> =
        read_zip_json_file(&mut zip_archive, BACKUP_HYPERLINKS_PATH)?;
    let relations: Vec<HyperlinkRelationBackupRow> =
        read_zip_json_file(&mut zip_archive, BACKUP_RELATIONS_PATH)?;
    let artifacts: Vec<HyperlinkArtifactBackupRow> =
        read_zip_json_file(&mut zip_archive, BACKUP_ARTIFACTS_PATH)?;
    let readability_progress: Vec<ReadabilityProgressBackupRow> =
        read_optional_zip_json_file(&mut zip_archive, BACKUP_READABILITY_PROGRESS_PATH)?
            .unwrap_or_default();

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
    if readability_progress.len() != manifest.readability_progress {
        return Err(format!(
            "readability_progress count mismatch: manifest says {}, file contains {}",
            manifest.readability_progress,
            readability_progress.len()
        ));
    }

    let mut seen_artifact_ids = HashSet::with_capacity(artifacts.len());
    for artifact in &artifacts {
        if !seen_artifact_ids.insert(artifact.id) {
            return Err(format!(
                "duplicate artifact id {} in artifacts.json",
                artifact.id
            ));
        }
        validate_archive_entry_path(&artifact.payload_path)?;
        let _entry = zip_archive
            .by_name(&artifact.payload_path)
            .map_err(|err| format!("missing {} in backup zip: {err}", artifact.payload_path))?;
    }

    let parsed_archive = AdminBackupArchive {
        hyperlinks,
        relations,
        artifacts,
        readability_progress,
    };

    restore_backup_archive(
        connection,
        &mut zip_archive,
        parsed_archive,
        report_progress,
    )
    .await
}

pub(crate) fn read_zip_json_file<T: DeserializeOwned, R: Read + Seek>(
    archive: &mut ZipArchive<R>,
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

fn read_optional_zip_json_file<T: DeserializeOwned, R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    path: &str,
) -> Result<Option<T>, String> {
    match archive.by_name(path) {
        Ok(mut entry) => {
            let mut content = String::new();
            entry
                .read_to_string(&mut content)
                .map_err(|err| format!("failed to read {path} from backup zip: {err}"))?;
            serde_json::from_str(&content)
                .map(Some)
                .map_err(|err| format!("failed to parse {path} from backup zip: {err}"))
        }
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(err) => Err(format!("failed to open {path} in backup zip: {err}")),
    }
}

pub(crate) fn read_zip_binary_file<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
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

async fn restore_backup_archive<F, R>(
    connection: &DatabaseConnection,
    zip_archive: &mut ZipArchive<R>,
    archive: AdminBackupArchive,
    mut report_progress: F,
) -> Result<ImportCompletionSummary, String>
where
    F: FnMut(ImportProgress),
    R: Read + Seek,
{
    let AdminBackupArchive {
        mut hyperlinks,
        mut relations,
        mut artifacts,
        mut readability_progress,
    } = archive;
    let hyperlink_total = hyperlinks.len();
    let relation_total = relations.len();
    let artifact_total = artifacts.len();
    let mut summary = ImportCompletionSummary {
        hyperlinks: 0,
        relations: 0,
        artifacts: 0,
    };

    hyperlinks.sort_by_key(|row| row.id);
    report_progress(ImportProgress {
        stage: ImportProgressStage::RestoringHyperlinks,
        hyperlinks_done: 0,
        hyperlinks_total: hyperlink_total,
        relations_done: 0,
        relations_total: relation_total,
        artifacts_done: 0,
        artifacts_total: artifact_total,
    });
    for (index, row) in hyperlinks.iter().enumerate() {
        restore_hyperlink_row(connection, row).await?;
        summary.hyperlinks += 1;
        report_progress(ImportProgress {
            stage: ImportProgressStage::RestoringHyperlinks,
            hyperlinks_done: index + 1,
            hyperlinks_total: hyperlink_total,
            relations_done: 0,
            relations_total: relation_total,
            artifacts_done: 0,
            artifacts_total: artifact_total,
        });
    }

    readability_progress.sort_by_key(|row| row.hyperlink_id);
    for row in &readability_progress {
        restore_readability_progress_row(connection, row).await?;
    }

    relations.sort_by_key(|row| row.id);
    report_progress(ImportProgress {
        stage: ImportProgressStage::RestoringRelations,
        hyperlinks_done: summary.hyperlinks,
        hyperlinks_total: hyperlink_total,
        relations_done: 0,
        relations_total: relation_total,
        artifacts_done: 0,
        artifacts_total: artifact_total,
    });
    for (index, row) in relations.iter().enumerate() {
        restore_relation_row(connection, row).await?;
        summary.relations += 1;
        report_progress(ImportProgress {
            stage: ImportProgressStage::RestoringRelations,
            hyperlinks_done: summary.hyperlinks,
            hyperlinks_total: hyperlink_total,
            relations_done: index + 1,
            relations_total: relation_total,
            artifacts_done: 0,
            artifacts_total: artifact_total,
        });
    }

    artifacts.sort_by_key(|row| row.id);
    report_progress(ImportProgress {
        stage: ImportProgressStage::RestoringArtifacts,
        hyperlinks_done: summary.hyperlinks,
        hyperlinks_total: hyperlink_total,
        relations_done: summary.relations,
        relations_total: relation_total,
        artifacts_done: 0,
        artifacts_total: artifact_total,
    });
    for (index, row) in artifacts.iter().enumerate() {
        let payload = read_zip_binary_file(zip_archive, &row.payload_path)?;
        restore_artifact_row(connection, row, &payload).await?;
        summary.artifacts += 1;
        report_progress(ImportProgress {
            stage: ImportProgressStage::RestoringArtifacts,
            hyperlinks_done: summary.hyperlinks,
            hyperlinks_total: hyperlink_total,
            relations_done: summary.relations,
            relations_total: relation_total,
            artifacts_done: index + 1,
            artifacts_total: artifact_total,
        });
    }

    report_progress(ImportProgress {
        stage: ImportProgressStage::Finalizing,
        hyperlinks_done: summary.hyperlinks,
        hyperlinks_total: hyperlink_total,
        relations_done: summary.relations,
        relations_total: relation_total,
        artifacts_done: summary.artifacts,
        artifacts_total: artifact_total,
    });

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
        active.summary = Set(row.summary.clone());
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
            summary: Set(row.summary.clone()),
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

async fn restore_readability_progress_row(
    connection: &DatabaseConnection,
    row: &ReadabilityProgressBackupRow,
) -> Result<(), String> {
    let updated_at = parse_datetime(&row.updated_at).map_err(|err| {
        format!(
            "invalid updated_at for readability progress hyperlink {}: {err}",
            row.hyperlink_id
        )
    })?;
    if !row.progress.is_finite() {
        return Err(format!(
            "invalid progress for readability progress hyperlink {}: must be finite",
            row.hyperlink_id
        ));
    }
    if !(0.0..=1.0).contains(&row.progress) {
        return Err(format!(
            "invalid progress for readability progress hyperlink {}: must be between 0.0 and 1.0",
            row.hyperlink_id
        ));
    }

    readability_progress::restore(connection, row.hyperlink_id, row.progress, updated_at)
        .await
        .map_err(|err| {
            format!(
                "failed to restore readability progress for hyperlink {}: {err}",
                row.hyperlink_id
            )
        })?;

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
