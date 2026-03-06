use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicU64, Ordering},
};

use sea_orm::entity::prelude::{DateTime, DateTimeUtc};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

#[derive(Clone, Copy, Debug)]
pub(crate) struct TagReclassifyProgress {
    pub(crate) processed: usize,
    pub(crate) total: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TagReclassifyCompletionSummary {
    pub(crate) processed: usize,
    pub(crate) total: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct TagReclassifyStatusResponse {
    pub(crate) state: String,
    pub(crate) job_id: Option<u64>,
    pub(crate) started_at: Option<String>,
    pub(crate) finished_at: Option<String>,
    pub(crate) processed: Option<usize>,
    pub(crate) total: Option<usize>,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct StartTagReclassifyResult {
    pub(crate) started: bool,
    pub(crate) status: TagReclassifyStatusResponse,
}

#[derive(Clone, Debug, Default)]
pub struct AdminTagReclassifyManager {
    inner: Arc<Mutex<AdminTagReclassifyInner>>,
}

#[derive(Debug)]
struct AdminTagReclassifyInner {
    next_job_id: u64,
    status: TagReclassifyStatus,
    running_handle: Option<JoinHandle<()>>,
}

impl Default for AdminTagReclassifyInner {
    fn default() -> Self {
        Self {
            next_job_id: 1,
            status: TagReclassifyStatus::Idle,
            running_handle: None,
        }
    }
}

#[derive(Clone, Debug)]
enum TagReclassifyStatus {
    Idle,
    Running(TagReclassifyRunningStatus),
    Ready(TagReclassifyReadyStatus),
    Failed(TagReclassifyTerminalStatus),
    Cancelled(TagReclassifyTerminalStatus),
}

#[derive(Clone, Debug)]
struct TagReclassifyRunningStatus {
    job_id: u64,
    started_at: DateTime,
    progress: TagReclassifyProgress,
}

#[derive(Clone, Debug)]
struct TagReclassifyReadyStatus {
    job_id: u64,
    started_at: DateTime,
    finished_at: DateTime,
    summary: TagReclassifyCompletionSummary,
}

#[derive(Clone, Debug)]
struct TagReclassifyTerminalStatus {
    job_id: u64,
    started_at: DateTime,
    finished_at: DateTime,
    error: Option<String>,
    progress: TagReclassifyProgress,
}

static RECLASSIFY_COUNTER: AtomicU64 = AtomicU64::new(1);

impl AdminTagReclassifyManager {
    pub(crate) fn snapshot(&self) -> TagReclassifyStatusResponse {
        let inner = self.lock_inner();
        status_to_response(&inner.status)
    }

    pub(crate) fn start_job<F>(&self, spawn_fn: F) -> StartTagReclassifyResult
    where
        F: FnOnce(u64) -> JoinHandle<()>,
    {
        let mut inner = self.lock_inner();
        if matches!(inner.status, TagReclassifyStatus::Running(_)) {
            return StartTagReclassifyResult {
                started: false,
                status: status_to_response(&inner.status),
            };
        }

        let fallback = RECLASSIFY_COUNTER.fetch_add(1, Ordering::Relaxed);
        let job_id = inner.next_job_id.max(fallback);
        inner.next_job_id = job_id.saturating_add(1);
        inner.status = TagReclassifyStatus::Running(TagReclassifyRunningStatus {
            job_id,
            started_at: now_utc(),
            progress: TagReclassifyProgress {
                processed: 0,
                total: 0,
            },
        });
        inner.running_handle = Some(spawn_fn(job_id));

        StartTagReclassifyResult {
            started: true,
            status: status_to_response(&inner.status),
        }
    }

    pub(crate) fn update_progress(&self, job_id: u64, progress: TagReclassifyProgress) {
        let mut inner = self.lock_inner();
        let TagReclassifyStatus::Running(status) = &mut inner.status else {
            return;
        };
        if status.job_id != job_id {
            return;
        }
        status.progress = progress;
    }

    pub(crate) fn mark_ready(&self, job_id: u64, summary: TagReclassifyCompletionSummary) {
        let mut inner = self.lock_inner();
        let TagReclassifyStatus::Running(status) = &inner.status else {
            return;
        };
        if status.job_id != job_id {
            return;
        }

        inner.status = TagReclassifyStatus::Ready(TagReclassifyReadyStatus {
            job_id,
            started_at: status.started_at,
            finished_at: now_utc(),
            summary,
        });
        inner.running_handle = None;
    }

    pub(crate) fn mark_failed(&self, job_id: u64, error: String, progress: TagReclassifyProgress) {
        let mut inner = self.lock_inner();
        let TagReclassifyStatus::Running(status) = &inner.status else {
            return;
        };
        if status.job_id != job_id {
            return;
        }

        inner.status = TagReclassifyStatus::Failed(TagReclassifyTerminalStatus {
            job_id,
            started_at: status.started_at,
            finished_at: now_utc(),
            error: Some(error),
            progress,
        });
        inner.running_handle = None;
    }

    pub(crate) fn cancel_running(&self) -> bool {
        let mut inner = self.lock_inner();
        let TagReclassifyStatus::Running(status) = &inner.status else {
            return false;
        };

        let terminal = TagReclassifyTerminalStatus {
            job_id: status.job_id,
            started_at: status.started_at,
            finished_at: now_utc(),
            error: None,
            progress: status.progress,
        };
        if let Some(handle) = inner.running_handle.take() {
            handle.abort();
        }
        inner.status = TagReclassifyStatus::Cancelled(terminal);
        true
    }

    fn lock_inner(&self) -> MutexGuard<'_, AdminTagReclassifyInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

fn status_to_response(status: &TagReclassifyStatus) -> TagReclassifyStatusResponse {
    match status {
        TagReclassifyStatus::Idle => TagReclassifyStatusResponse {
            state: "idle".to_string(),
            job_id: None,
            started_at: None,
            finished_at: None,
            processed: None,
            total: None,
            error: None,
        },
        TagReclassifyStatus::Running(running) => TagReclassifyStatusResponse {
            state: "running".to_string(),
            job_id: Some(running.job_id),
            started_at: Some(format_datetime(&running.started_at)),
            finished_at: None,
            processed: Some(running.progress.processed),
            total: Some(running.progress.total),
            error: None,
        },
        TagReclassifyStatus::Ready(ready) => TagReclassifyStatusResponse {
            state: "ready".to_string(),
            job_id: Some(ready.job_id),
            started_at: Some(format_datetime(&ready.started_at)),
            finished_at: Some(format_datetime(&ready.finished_at)),
            processed: Some(ready.summary.processed),
            total: Some(ready.summary.total),
            error: None,
        },
        TagReclassifyStatus::Failed(failed) => TagReclassifyStatusResponse {
            state: "failed".to_string(),
            job_id: Some(failed.job_id),
            started_at: Some(format_datetime(&failed.started_at)),
            finished_at: Some(format_datetime(&failed.finished_at)),
            processed: Some(failed.progress.processed),
            total: Some(failed.progress.total),
            error: failed.error.clone(),
        },
        TagReclassifyStatus::Cancelled(cancelled) => TagReclassifyStatusResponse {
            state: "cancelled".to_string(),
            job_id: Some(cancelled.job_id),
            started_at: Some(format_datetime(&cancelled.started_at)),
            finished_at: Some(format_datetime(&cancelled.finished_at)),
            processed: Some(cancelled.progress.processed),
            total: Some(cancelled.progress.total),
            error: cancelled.error.clone(),
        },
    }
}

fn now_utc() -> DateTime {
    DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}

fn format_datetime(value: &DateTime) -> String {
    value.format("%Y-%m-%dT%H:%M:%S%.fZ").to_string()
}
