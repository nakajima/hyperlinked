use std::{
    sync::{Arc, Mutex, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use lilqueue::{
    BackoffStrategy, Job, JobError, ProcessorOptions, SqliteJobProcessor, WorkerHandle,
    dashboard::{DashboardRuntimeState, DashboardWakeResult},
};
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};

const DEFAULT_WORKER_MAX_ATTEMPTS: u32 = 20;
const DEFAULT_WORKER_LOCK_TIMEOUT_SECS: u64 = 300;
const DEFAULT_WORKER_POLL_INTERVAL_MS: u64 = 250;

static WORKER_RUNTIME: RwLock<Option<WorkerRuntime>> = RwLock::new(None);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessingTask {
    pub processing_job_id: i32,
}

#[async_trait::async_trait]
impl Job for ProcessingTask {
    async fn process(&self) -> Result<(), JobError> {
        let runtime = {
            let guard = WORKER_RUNTIME
                .read()
                .map_err(|_| JobError::retryable("worker runtime lock poisoned"))?;
            guard
                .clone()
                .ok_or_else(|| JobError::retryable("worker runtime not initialized"))?
        };

        crate::processors::worker::process_job(
            &runtime.connection,
            &runtime.queue,
            self.processing_job_id,
        )
        .await
        .map_err(|err| {
            JobError::retryable(format!(
                "failed to process hyperlink job {}: {err}",
                self.processing_job_id
            ))
        })?;

        Ok(())
    }
}

#[derive(Clone)]
struct WorkerRuntime {
    connection: DatabaseConnection,
    queue: ProcessingQueue,
}

#[derive(Clone)]
pub struct ProcessingQueue {
    processor: SqliteJobProcessor<ProcessingTask>,
    worker_handle: Arc<Mutex<Option<WorkerHandle>>>,
    configured_concurrency: usize,
    last_wake: Arc<Mutex<Option<DashboardWakeResult>>>,
}

impl std::fmt::Debug for ProcessingQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessingQueue").finish_non_exhaustive()
    }
}

impl ProcessingQueue {
    pub async fn connect(connection: DatabaseConnection) -> Result<Self, String> {
        let options = ProcessorOptions {
            max_attempts: env_u32(
                "PROCESSING_WORKER_MAX_ATTEMPTS",
                DEFAULT_WORKER_MAX_ATTEMPTS,
                1,
                1000,
            ),
            lock_timeout: Duration::from_secs(env_u64(
                "PROCESSING_WORKER_LOCK_TIMEOUT_SECS",
                DEFAULT_WORKER_LOCK_TIMEOUT_SECS,
                1,
                3600,
            )),
            poll_interval: Duration::from_millis(env_u64(
                "PROCESSING_WORKER_POLL_INTERVAL_MS",
                DEFAULT_WORKER_POLL_INTERVAL_MS,
                10,
                5000,
            )),
            backoff: BackoffStrategy::Exponential {
                base: Duration::from_secs(1),
                max: Duration::from_secs(60),
            },
        };

        let processor = SqliteJobProcessor::<ProcessingTask>::new(connection, options)
            .await
            .map_err(|err| format!("failed to initialize sqlite processing queue: {err}"))?;
        let configured_concurrency = worker_concurrency();

        Ok(Self {
            processor,
            worker_handle: Arc::new(Mutex::new(None)),
            configured_concurrency,
            last_wake: Arc::new(Mutex::new(None)),
        })
    }

    pub fn dashboard_db(&self) -> DatabaseConnection {
        self.processor.db().clone()
    }

    pub async fn enqueue_processing_job(&self, processing_job_id: i32) -> Result<(), String> {
        self.processor
            .enqueue(&ProcessingTask { processing_job_id })
            .await
            .map_err(|err| format!("failed to enqueue processing task {processing_job_id}: {err}"))
            .map(|_| ())
    }

    pub async fn spawn_worker(&self, connection: DatabaseConnection) -> Result<(), String> {
        let runtime = WorkerRuntime {
            connection,
            queue: self.clone(),
        };

        {
            let mut guard = WORKER_RUNTIME
                .write()
                .map_err(|_| "failed to set worker runtime: lock poisoned".to_string())?;
            *guard = Some(runtime);
        }

        let new_handle = self.processor.spawn_workers(self.configured_concurrency);

        let old_handle = {
            let mut guard = self
                .worker_handle
                .lock()
                .map_err(|_| "failed to store worker handle: lock poisoned".to_string())?;
            guard.replace(new_handle)
        };

        if let Some(old) = old_handle {
            old.shutdown_and_wait().await;
        }

        Ok(())
    }

    pub async fn shutdown_worker(&self) -> Result<(), String> {
        let handle = {
            let mut guard = self
                .worker_handle
                .lock()
                .map_err(|_| "failed to access worker handle: lock poisoned".to_string())?;
            guard.take()
        };

        if let Some(handle) = handle {
            handle.shutdown_and_wait().await;
        }

        Ok(())
    }

    fn workers_running(&self) -> Result<bool, String> {
        let guard = self
            .worker_handle
            .lock()
            .map_err(|_| "failed to access worker handle: lock poisoned".to_string())?;
        Ok(guard.is_some())
    }

    pub fn wake_workers(&self) -> Result<DashboardWakeResult, String> {
        let workers_running = self.workers_running()?;
        self.processor.wake_workers();

        let result = DashboardWakeResult {
            at_epoch_s: now_epoch_seconds(),
            result: if workers_running {
                "wake signal sent".to_string()
            } else {
                "wake requested but workers are not running".to_string()
            },
        };

        let mut guard = self
            .last_wake
            .lock()
            .map_err(|_| "failed to store wake result: lock poisoned".to_string())?;
        *guard = Some(result.clone());

        Ok(result)
    }

    pub fn dashboard_runtime_state(&self) -> Result<DashboardRuntimeState, String> {
        let workers_running = self.workers_running()?;
        let last_wake = self
            .last_wake
            .lock()
            .map_err(|_| "failed to access wake result: lock poisoned".to_string())?
            .clone();

        Ok(DashboardRuntimeState {
            workers_running,
            configured_concurrency: self.configured_concurrency,
            last_wake_at_epoch_s: last_wake.as_ref().map(|event| event.at_epoch_s),
            last_wake_result: last_wake.map(|event| event.result),
        })
    }
}

#[async_trait::async_trait]
impl lilqueue::dashboard::DashboardControl for ProcessingQueue {
    async fn wake_workers(&self) -> Result<DashboardWakeResult, String> {
        ProcessingQueue::wake_workers(self)
    }

    async fn runtime_state(&self) -> Result<DashboardRuntimeState, String> {
        ProcessingQueue::dashboard_runtime_state(self)
    }
}

fn env_u32(key: &str, default: u32, min: u32, max: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .map(|value| value.clamp(min, max))
        .unwrap_or(default.clamp(min, max))
}

fn env_u64(key: &str, default: u64, min: u64, max: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value.clamp(min, max))
        .unwrap_or(default.clamp(min, max))
}

fn worker_concurrency() -> usize {
    env_u32("PROCESSING_WORKER_CONCURRENCY", 1, 1, 32) as usize
}

fn now_epoch_seconds() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    i64::try_from(secs).unwrap_or(i64::MAX)
}
