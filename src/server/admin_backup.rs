use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use sea_orm::entity::prelude::{DateTime, DateTimeUtc};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

#[derive(Clone, Copy, Debug)]
pub(crate) enum BackupProgressStage {
    LoadingRecords,
    PackingArtifacts,
    Finalizing,
}

impl BackupProgressStage {
    fn as_str(self) -> &'static str {
        match self {
            Self::LoadingRecords => "loading_records",
            Self::PackingArtifacts => "packing_artifacts",
            Self::Finalizing => "finalizing",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BackupProgress {
    pub(crate) stage: BackupProgressStage,
    pub(crate) artifacts_done: usize,
    pub(crate) artifacts_total: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BackupCompletionSummary {
    pub(crate) hyperlinks: usize,
    pub(crate) relations: usize,
    pub(crate) artifacts: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct BackupStatusResponse {
    pub(crate) state: String,
    pub(crate) job_id: Option<u64>,
    pub(crate) started_at: Option<String>,
    pub(crate) finished_at: Option<String>,
    pub(crate) stage: Option<String>,
    pub(crate) artifacts_done: Option<usize>,
    pub(crate) artifacts_total: Option<usize>,
    pub(crate) hyperlinks: Option<usize>,
    pub(crate) relations: Option<usize>,
    pub(crate) artifacts: Option<usize>,
    pub(crate) error: Option<String>,
    pub(crate) download_ready: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct StartBackupResult {
    pub(crate) started: bool,
    pub(crate) status: BackupStatusResponse,
}

#[derive(Clone, Debug, Default)]
pub struct AdminBackupManager {
    inner: Arc<Mutex<AdminBackupInner>>,
}

#[derive(Debug)]
struct AdminBackupInner {
    next_job_id: u64,
    status: BackupStatus,
    running_handle: Option<JoinHandle<()>>,
    ready_file_path: Option<PathBuf>,
}

impl Default for AdminBackupInner {
    fn default() -> Self {
        Self {
            next_job_id: 1,
            status: BackupStatus::Idle,
            running_handle: None,
            ready_file_path: None,
        }
    }
}

#[derive(Clone, Debug)]
enum BackupStatus {
    Idle,
    Running(BackupRunningStatus),
    Ready(BackupReadyStatus),
    Failed(BackupTerminalStatus),
    Cancelled(BackupTerminalStatus),
}

#[derive(Clone, Debug)]
struct BackupRunningStatus {
    job_id: u64,
    started_at: DateTime,
    output_path: PathBuf,
    stage: BackupProgressStage,
    artifacts_done: usize,
    artifacts_total: usize,
}

#[derive(Clone, Debug)]
struct BackupReadyStatus {
    job_id: u64,
    started_at: DateTime,
    finished_at: DateTime,
    summary: BackupCompletionSummary,
}

#[derive(Clone, Debug)]
struct BackupTerminalStatus {
    job_id: u64,
    started_at: DateTime,
    finished_at: DateTime,
    error: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum BackupDownloadError {
    NotReady,
    MissingPayload,
}

const BACKUP_FILE_PREFIX: &str = "hyperlinked-backup-";
const BACKUP_FILE_SUFFIX: &str = ".zip";
static BACKUP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);
#[cfg(not(test))]
const BACKUP_LATEST_POINTER_FILE: &str = "hyperlinked-backup-latest-path.txt";

impl AdminBackupManager {
    pub(crate) fn snapshot(&self) -> BackupStatusResponse {
        let inner = self.lock_inner();
        status_to_response(
            &inner.status,
            download_file_path_from_inner(&inner).is_some(),
        )
    }

    pub(crate) fn start_job<F>(&self, spawn_fn: F) -> StartBackupResult
    where
        F: FnOnce(u64, PathBuf) -> JoinHandle<()>,
    {
        let mut inner = self.lock_inner();
        if matches!(inner.status, BackupStatus::Running(_)) {
            return StartBackupResult {
                started: false,
                status: status_to_response(
                    &inner.status,
                    download_file_path_from_inner(&inner).is_some(),
                ),
            };
        }

        let job_id = inner.next_job_id;
        inner.next_job_id = inner.next_job_id.saturating_add(1);
        let started_at = now_utc();
        let output_path = backup_output_path(job_id);
        delete_backup_file_if_exists(&output_path);
        inner.status = BackupStatus::Running(BackupRunningStatus {
            job_id,
            started_at,
            output_path: output_path.clone(),
            stage: BackupProgressStage::LoadingRecords,
            artifacts_done: 0,
            artifacts_total: 0,
        });

        let handle = spawn_fn(job_id, output_path);
        inner.running_handle = Some(handle);

        StartBackupResult {
            started: true,
            status: status_to_response(
                &inner.status,
                download_file_path_from_inner(&inner).is_some(),
            ),
        }
    }

    pub(crate) fn update_progress(&self, job_id: u64, progress: BackupProgress) {
        let mut inner = self.lock_inner();
        let BackupStatus::Running(status) = &mut inner.status else {
            return;
        };
        if status.job_id != job_id {
            return;
        }

        status.stage = progress.stage;
        status.artifacts_done = progress.artifacts_done;
        status.artifacts_total = progress.artifacts_total;
    }

    pub(crate) fn mark_ready(&self, job_id: u64, summary: BackupCompletionSummary) {
        let mut inner = self.lock_inner();
        let BackupStatus::Running(status) = &inner.status else {
            return;
        };
        if status.job_id != job_id {
            return;
        }

        let output_path = status.output_path.clone();
        let started_at = status.started_at;
        let previous_ready_path = inner.ready_file_path.replace(output_path.clone());
        inner.status = BackupStatus::Ready(BackupReadyStatus {
            job_id,
            started_at,
            finished_at: now_utc(),
            summary,
        });
        inner.running_handle = None;
        write_latest_backup_pointer(&output_path);
        if let Some(previous_ready_path) = previous_ready_path
            && previous_ready_path != output_path
        {
            delete_backup_file_if_exists(&previous_ready_path);
        }
    }

    pub(crate) fn mark_failed(&self, job_id: u64, error: String) {
        let mut inner = self.lock_inner();
        let BackupStatus::Running(status) = &inner.status else {
            return;
        };
        if status.job_id != job_id {
            return;
        }

        let output_path = status.output_path.clone();
        let started_at = status.started_at;
        inner.status = BackupStatus::Failed(BackupTerminalStatus {
            job_id,
            started_at,
            finished_at: now_utc(),
            error: Some(error),
        });
        inner.running_handle = None;
        delete_backup_file_if_exists(&output_path);
    }

    pub(crate) fn cancel_running(&self) -> bool {
        let mut inner = self.lock_inner();
        let BackupStatus::Running(status) = &inner.status else {
            return false;
        };

        let job_id = status.job_id;
        let started_at = status.started_at;
        let output_path = status.output_path.clone();
        if let Some(handle) = inner.running_handle.take() {
            handle.abort();
        }

        inner.status = BackupStatus::Cancelled(BackupTerminalStatus {
            job_id,
            started_at,
            finished_at: now_utc(),
            error: None,
        });
        delete_backup_file_if_exists(&output_path);
        true
    }

    pub(crate) fn download_file_path(&self) -> Result<PathBuf, BackupDownloadError> {
        let inner = self.lock_inner();
        if let Some(path) = download_file_path_from_inner(&inner) {
            return Ok(path);
        }
        if matches!(inner.status, BackupStatus::Ready(_)) {
            return Err(BackupDownloadError::MissingPayload);
        }
        Err(BackupDownloadError::NotReady)
    }

    fn lock_inner(&self) -> MutexGuard<'_, AdminBackupInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

fn now_utc() -> DateTime {
    DateTimeUtc::from(SystemTime::now()).naive_utc()
}

fn backup_output_path(job_id: u64) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = BACKUP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    std::env::temp_dir().join(format!(
        "{BACKUP_FILE_PREFIX}{timestamp}-{pid}-{job_id}-{sequence}{BACKUP_FILE_SUFFIX}"
    ))
}

fn delete_backup_file_if_exists(path: &PathBuf) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), error = %error, "failed to delete backup file");
    }
}

fn download_file_path_from_inner(inner: &AdminBackupInner) -> Option<PathBuf> {
    if let Some(path) = inner.ready_file_path.as_ref()
        && backup_file_exists(path)
    {
        return Some(path.clone());
    }
    latest_backup_file_from_pointer()
}

fn backup_file_exists(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

#[cfg(test)]
fn latest_backup_file_from_pointer() -> Option<PathBuf> {
    None
}

#[cfg(not(test))]
fn latest_backup_file_from_pointer() -> Option<PathBuf> {
    let pointer_path = backup_latest_pointer_path();
    let raw = std::fs::read_to_string(pointer_path).ok()?;
    let candidate = PathBuf::from(raw.trim());
    if !matches_backup_file_name(candidate.as_path()) {
        return None;
    }
    let temp_dir = std::env::temp_dir();
    if candidate.parent() != Some(temp_dir.as_path()) {
        return None;
    }
    backup_file_exists(&candidate).then_some(candidate)
}

#[cfg(not(test))]
fn backup_latest_pointer_path() -> PathBuf {
    std::env::temp_dir().join(BACKUP_LATEST_POINTER_FILE)
}

#[cfg(test)]
fn write_latest_backup_pointer(_path: &Path) {}

#[cfg(not(test))]
fn write_latest_backup_pointer(path: &Path) {
    if let Err(error) = std::fs::write(backup_latest_pointer_path(), path.display().to_string()) {
        tracing::warn!(
            path = %path.display(),
            error = %error,
            "failed to persist backup latest pointer"
        );
    }
}

#[cfg(not(test))]
fn matches_backup_file_name(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    file_name.starts_with(BACKUP_FILE_PREFIX) && file_name.ends_with(BACKUP_FILE_SUFFIX)
}

fn format_datetime(value: &DateTime) -> String {
    value.format("%Y-%m-%dT%H:%M:%S%.fZ").to_string()
}

fn status_to_response(status: &BackupStatus, has_payload: bool) -> BackupStatusResponse {
    match status {
        BackupStatus::Idle => BackupStatusResponse {
            state: "idle".to_string(),
            job_id: None,
            started_at: None,
            finished_at: None,
            stage: None,
            artifacts_done: None,
            artifacts_total: None,
            hyperlinks: None,
            relations: None,
            artifacts: None,
            error: None,
            download_ready: has_payload,
        },
        BackupStatus::Running(running) => BackupStatusResponse {
            state: "running".to_string(),
            job_id: Some(running.job_id),
            started_at: Some(format_datetime(&running.started_at)),
            finished_at: None,
            stage: Some(running.stage.as_str().to_string()),
            artifacts_done: Some(running.artifacts_done),
            artifacts_total: Some(running.artifacts_total),
            hyperlinks: None,
            relations: None,
            artifacts: None,
            error: None,
            download_ready: has_payload,
        },
        BackupStatus::Ready(ready) => BackupStatusResponse {
            state: "ready".to_string(),
            job_id: Some(ready.job_id),
            started_at: Some(format_datetime(&ready.started_at)),
            finished_at: Some(format_datetime(&ready.finished_at)),
            stage: None,
            artifacts_done: None,
            artifacts_total: None,
            hyperlinks: Some(ready.summary.hyperlinks),
            relations: Some(ready.summary.relations),
            artifacts: Some(ready.summary.artifacts),
            error: None,
            download_ready: has_payload,
        },
        BackupStatus::Failed(failed) => BackupStatusResponse {
            state: "failed".to_string(),
            job_id: Some(failed.job_id),
            started_at: Some(format_datetime(&failed.started_at)),
            finished_at: Some(format_datetime(&failed.finished_at)),
            stage: None,
            artifacts_done: None,
            artifacts_total: None,
            hyperlinks: None,
            relations: None,
            artifacts: None,
            error: failed.error.clone(),
            download_ready: has_payload,
        },
        BackupStatus::Cancelled(cancelled) => BackupStatusResponse {
            state: "cancelled".to_string(),
            job_id: Some(cancelled.job_id),
            started_at: Some(format_datetime(&cancelled.started_at)),
            finished_at: Some(format_datetime(&cancelled.finished_at)),
            stage: None,
            artifacts_done: None,
            artifacts_total: None,
            hyperlinks: None,
            relations: None,
            artifacts: None,
            error: None,
            download_ready: has_payload,
        },
    }
}
