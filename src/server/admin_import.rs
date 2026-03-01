use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use sea_orm::entity::prelude::{DateTime, DateTimeUtc};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

#[derive(Clone, Copy, Debug)]
pub(crate) enum ImportProgressStage {
    Validating,
    RestoringHyperlinks,
    RestoringRelations,
    RestoringArtifacts,
    Finalizing,
}

impl ImportProgressStage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Validating => "validating",
            Self::RestoringHyperlinks => "restoring_hyperlinks",
            Self::RestoringRelations => "restoring_relations",
            Self::RestoringArtifacts => "restoring_artifacts",
            Self::Finalizing => "finalizing",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ImportProgress {
    pub(crate) stage: ImportProgressStage,
    pub(crate) hyperlinks_done: usize,
    pub(crate) hyperlinks_total: usize,
    pub(crate) relations_done: usize,
    pub(crate) relations_total: usize,
    pub(crate) artifacts_done: usize,
    pub(crate) artifacts_total: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ImportCompletionSummary {
    pub(crate) hyperlinks: usize,
    pub(crate) relations: usize,
    pub(crate) artifacts: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ImportStatusResponse {
    pub(crate) state: String,
    pub(crate) job_id: Option<u64>,
    pub(crate) started_at: Option<String>,
    pub(crate) finished_at: Option<String>,
    pub(crate) stage: Option<String>,
    pub(crate) hyperlinks_done: Option<usize>,
    pub(crate) hyperlinks_total: Option<usize>,
    pub(crate) relations_done: Option<usize>,
    pub(crate) relations_total: Option<usize>,
    pub(crate) artifacts_done: Option<usize>,
    pub(crate) artifacts_total: Option<usize>,
    pub(crate) hyperlinks: Option<usize>,
    pub(crate) relations: Option<usize>,
    pub(crate) artifacts: Option<usize>,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct StartImportResult {
    pub(crate) started: bool,
    pub(crate) status: ImportStatusResponse,
}

#[derive(Clone, Debug, Default)]
pub struct AdminImportManager {
    inner: Arc<Mutex<AdminImportInner>>,
}

#[derive(Debug)]
struct AdminImportInner {
    next_job_id: u64,
    status: ImportStatus,
    running_handle: Option<JoinHandle<()>>,
}

impl Default for AdminImportInner {
    fn default() -> Self {
        Self {
            next_job_id: 1,
            status: ImportStatus::Idle,
            running_handle: None,
        }
    }
}

#[derive(Clone, Debug)]
enum ImportStatus {
    Idle,
    Running(ImportRunningStatus),
    Ready(ImportReadyStatus),
    Failed(ImportTerminalStatus),
    Cancelled(ImportTerminalStatus),
}

#[derive(Clone, Debug)]
struct ImportRunningStatus {
    job_id: u64,
    started_at: DateTime,
    archive_path: PathBuf,
    progress: ImportProgress,
}

#[derive(Clone, Debug)]
struct ImportReadyStatus {
    job_id: u64,
    started_at: DateTime,
    finished_at: DateTime,
    summary: ImportCompletionSummary,
}

#[derive(Clone, Debug)]
struct ImportTerminalStatus {
    job_id: u64,
    started_at: DateTime,
    finished_at: DateTime,
    error: Option<String>,
}

const IMPORT_FILE_PREFIX: &str = "hyperlinked-import-";
const IMPORT_FILE_SUFFIX: &str = ".zip";
static IMPORT_FILE_COUNTER: AtomicU64 = AtomicU64::new(1);

impl AdminImportManager {
    pub(crate) fn snapshot(&self) -> ImportStatusResponse {
        let inner = self.lock_inner();
        status_to_response(&inner.status)
    }

    pub(crate) fn start_job<F>(&self, archive_path: PathBuf, spawn_fn: F) -> StartImportResult
    where
        F: FnOnce(u64, PathBuf) -> JoinHandle<()>,
    {
        let mut inner = self.lock_inner();
        if matches!(inner.status, ImportStatus::Running(_)) {
            delete_import_file_if_exists(&archive_path);
            return StartImportResult {
                started: false,
                status: status_to_response(&inner.status),
            };
        }

        let job_id = inner.next_job_id;
        inner.next_job_id = inner.next_job_id.saturating_add(1);
        inner.status = ImportStatus::Running(ImportRunningStatus {
            job_id,
            started_at: now_utc(),
            archive_path: archive_path.clone(),
            progress: ImportProgress {
                stage: ImportProgressStage::Validating,
                hyperlinks_done: 0,
                hyperlinks_total: 0,
                relations_done: 0,
                relations_total: 0,
                artifacts_done: 0,
                artifacts_total: 0,
            },
        });
        inner.running_handle = Some(spawn_fn(job_id, archive_path));

        StartImportResult {
            started: true,
            status: status_to_response(&inner.status),
        }
    }

    pub(crate) fn update_progress(&self, job_id: u64, progress: ImportProgress) {
        let mut inner = self.lock_inner();
        let ImportStatus::Running(status) = &mut inner.status else {
            return;
        };
        if status.job_id != job_id {
            return;
        }
        status.progress = progress;
    }

    pub(crate) fn mark_ready(&self, job_id: u64, summary: ImportCompletionSummary) {
        let mut inner = self.lock_inner();
        let ImportStatus::Running(status) = &inner.status else {
            return;
        };
        if status.job_id != job_id {
            return;
        }

        let started_at = status.started_at;
        let archive_path = status.archive_path.clone();
        inner.status = ImportStatus::Ready(ImportReadyStatus {
            job_id,
            started_at,
            finished_at: now_utc(),
            summary,
        });
        inner.running_handle = None;
        delete_import_file_if_exists(&archive_path);
    }

    pub(crate) fn mark_failed(&self, job_id: u64, error: String) {
        let mut inner = self.lock_inner();
        let ImportStatus::Running(status) = &inner.status else {
            return;
        };
        if status.job_id != job_id {
            return;
        }

        let started_at = status.started_at;
        let archive_path = status.archive_path.clone();
        inner.status = ImportStatus::Failed(ImportTerminalStatus {
            job_id,
            started_at,
            finished_at: now_utc(),
            error: Some(error),
        });
        inner.running_handle = None;
        delete_import_file_if_exists(&archive_path);
    }

    pub(crate) fn cancel_running(&self) -> bool {
        let mut inner = self.lock_inner();
        let ImportStatus::Running(status) = &inner.status else {
            return false;
        };

        let job_id = status.job_id;
        let started_at = status.started_at;
        let archive_path = status.archive_path.clone();
        if let Some(handle) = inner.running_handle.take() {
            handle.abort();
        }
        inner.status = ImportStatus::Cancelled(ImportTerminalStatus {
            job_id,
            started_at,
            finished_at: now_utc(),
            error: None,
        });
        delete_import_file_if_exists(&archive_path);
        true
    }

    fn lock_inner(&self) -> MutexGuard<'_, AdminImportInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

pub(crate) fn next_import_upload_path() -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let pid = std::process::id();
    let serial = IMPORT_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "{IMPORT_FILE_PREFIX}{timestamp}-{pid}-{serial}{IMPORT_FILE_SUFFIX}"
    ))
}

fn now_utc() -> DateTime {
    DateTimeUtc::from(SystemTime::now()).naive_utc()
}

fn format_datetime(value: &DateTime) -> String {
    value.format("%Y-%m-%dT%H:%M:%S%.fZ").to_string()
}

fn delete_import_file_if_exists(path: &Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), error = %error, "failed to delete import file");
    }
}

fn status_to_response(status: &ImportStatus) -> ImportStatusResponse {
    match status {
        ImportStatus::Idle => ImportStatusResponse {
            state: "idle".to_string(),
            job_id: None,
            started_at: None,
            finished_at: None,
            stage: None,
            hyperlinks_done: None,
            hyperlinks_total: None,
            relations_done: None,
            relations_total: None,
            artifacts_done: None,
            artifacts_total: None,
            hyperlinks: None,
            relations: None,
            artifacts: None,
            error: None,
        },
        ImportStatus::Running(running) => ImportStatusResponse {
            state: "running".to_string(),
            job_id: Some(running.job_id),
            started_at: Some(format_datetime(&running.started_at)),
            finished_at: None,
            stage: Some(running.progress.stage.as_str().to_string()),
            hyperlinks_done: Some(running.progress.hyperlinks_done),
            hyperlinks_total: Some(running.progress.hyperlinks_total),
            relations_done: Some(running.progress.relations_done),
            relations_total: Some(running.progress.relations_total),
            artifacts_done: Some(running.progress.artifacts_done),
            artifacts_total: Some(running.progress.artifacts_total),
            hyperlinks: None,
            relations: None,
            artifacts: None,
            error: None,
        },
        ImportStatus::Ready(ready) => ImportStatusResponse {
            state: "ready".to_string(),
            job_id: Some(ready.job_id),
            started_at: Some(format_datetime(&ready.started_at)),
            finished_at: Some(format_datetime(&ready.finished_at)),
            stage: None,
            hyperlinks_done: Some(ready.summary.hyperlinks),
            hyperlinks_total: Some(ready.summary.hyperlinks),
            relations_done: Some(ready.summary.relations),
            relations_total: Some(ready.summary.relations),
            artifacts_done: Some(ready.summary.artifacts),
            artifacts_total: Some(ready.summary.artifacts),
            hyperlinks: Some(ready.summary.hyperlinks),
            relations: Some(ready.summary.relations),
            artifacts: Some(ready.summary.artifacts),
            error: None,
        },
        ImportStatus::Failed(failed) => ImportStatusResponse {
            state: "failed".to_string(),
            job_id: Some(failed.job_id),
            started_at: Some(format_datetime(&failed.started_at)),
            finished_at: Some(format_datetime(&failed.finished_at)),
            stage: None,
            hyperlinks_done: None,
            hyperlinks_total: None,
            relations_done: None,
            relations_total: None,
            artifacts_done: None,
            artifacts_total: None,
            hyperlinks: None,
            relations: None,
            artifacts: None,
            error: failed.error.clone(),
        },
        ImportStatus::Cancelled(cancelled) => ImportStatusResponse {
            state: "cancelled".to_string(),
            job_id: Some(cancelled.job_id),
            started_at: Some(format_datetime(&cancelled.started_at)),
            finished_at: Some(format_datetime(&cancelled.finished_at)),
            stage: None,
            hyperlinks_done: None,
            hyperlinks_total: None,
            relations_done: None,
            relations_total: None,
            artifacts_done: None,
            artifacts_total: None,
            hyperlinks: None,
            relations: None,
            artifacts: None,
            error: None,
        },
    }
}
