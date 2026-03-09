use chromiumoxide::{
    Page,
    browser::{Browser, BrowserConfig},
    cdp::browser_protocol::{
        emulation::{MediaFeature, SetDeviceMetricsOverrideParams},
        page::CaptureScreenshotFormat,
    },
    page::ScreenshotParams,
};
use futures_util::StreamExt;
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{RenderSettings as HayroRenderSettings, render as hayro_render};
use image::{GenericImageView, imageops::FilterType};
use reqwest::{Url, header::CONTENT_TYPE};
use sea_orm::DatabaseConnection;
use serde::Serialize;
use std::collections::HashSet;
use std::net::IpAddr;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::{net::lookup_host, time::sleep};

use crate::{
    entity::{
        hyperlink::{self, HyperlinkSourceType},
        hyperlink_artifact::HyperlinkArtifactKind,
    },
    model::{hyperlink_artifact, settings},
    processors::processor::{ProcessingError, Processor},
    server::font_diagnostics::{self, ScreenshotFontDiagnostics},
};

const MAX_HTML_BYTES: usize = 5 * 1024 * 1024;
const MAX_CSS_FILES: usize = 25;
const MAX_CSS_BYTES_PER_FILE: usize = 1024 * 1024;
const MAX_TOTAL_CSS_BYTES: usize = 20 * 1024 * 1024;

const SNAPSHOT_CONTENT_TYPE: &str = hyperlink_artifact::SNAPSHOT_WARC_GZIP_CONTENT_TYPE;
const SNAPSHOT_ERROR_CONTENT_TYPE: &str = "application/json";
const PDF_SOURCE_DEFAULT_CONTENT_TYPE: &str = "application/pdf";
const PDF_SIGNATURE: &[u8] = b"%PDF-";
const SCREENSHOT_CONTENT_TYPE: &str = "image/webp";
const SCREENSHOT_ERROR_CONTENT_TYPE: &str = "application/json";
const SCREENSHOT_WEBP_QUALITY: f32 = 85.0;
const SCREENSHOT_CAPTURE_WEBP_QUALITY: i64 = 85;

const RETRY_ATTEMPTS: usize = 4;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const SNAPSHOT_DEADLINE: Duration = Duration::from_secs(30);
const RETRY_BASE_BACKOFF_MS: u64 = 200;
const RETRY_JITTER_MAX_MS: u64 = 125;
const DEFAULT_SNAPSHOT_CONTENT_TIMEOUT_SECS: u64 = 20;
const DEFAULT_SNAPSHOT_CONTENT_RENDER_WAIT_MS: u64 = 5000;
const DEFAULT_SCREENSHOT_TIMEOUT_SECS: u64 = 20;
const CHROMIUM_PATH_ENV: &str = "CHROMIUM_PATH";
const CHROMIUM_RUNTIME_DIR_ENV: &str = "CHROMIUM_RUNTIME_DIR";
const DEFAULT_CHROMIUM_RUNTIME_DIR: &str = "hyperlinked-chromium-runtime";
const DEFAULT_SNAPSHOT_CONTENT_VIEWPORT: Viewport = Viewport {
    width: 1366,
    height: 4096,
};
const DEFAULT_SCREENSHOT_DESKTOP_VIEWPORT: Viewport = Viewport {
    width: 1366,
    height: 4096,
};
const DEFAULT_SCREENSHOT_THUMB_SIZE: u32 = 400;
const DEFAULT_SCREENSHOT_RENDER_WAIT_MS: u64 = 5000;
const DEFAULT_SCREENSHOT_MIN_PAGE_HEIGHT: u32 = 720;
const DEFAULT_SCREENSHOT_MAX_PAGE_HEIGHT: u32 = 12_000;
const DEFAULT_SCREENSHOT_EXACT_HEIGHT_ATTEMPTS: u64 = 3;
const DEFAULT_SCREENSHOT_FIXED_VIEWPORT_ATTEMPTS: u64 = 2;
const DEFAULT_SCREENSHOT_RETRY_BASE_BACKOFF_MS: u64 = 200;
const DEFAULT_SCREENSHOT_RETRY_JITTER_MAX_MS: u64 = 125;
const EXACT_HEIGHT_EMPTY_PAYLOAD_ERROR: &str =
    "chromium screenshot response contained an empty payload";
const SCREENSHOT_PROFILE_CLEANUP_RETRY_ATTEMPTS: usize = 4;
const SCREENSHOT_PROFILE_CLEANUP_RETRY_BASE_DELAY_MS: u64 = 50;
const SCREENSHOT_PROFILE_STALE_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const CHROMIUM_SHUTDOWN_WAIT_TIMEOUT: Duration = Duration::from_secs(2);

static SCREENSHOT_PROFILE_SWEEP_ONCE: OnceLock<()> = OnceLock::new();

pub struct SnapshotFetcher {
    job_id: i32,
}

pub struct SnapshotFetchOutput {
    pub source_artifact_id: i32,
    pub source_artifact_kind: HyperlinkArtifactKind,
    pub screenshot_artifact_id: Option<i32>,
    pub screenshot_dark_artifact_id: Option<i32>,
    pub screenshot_thumb_artifact_id: Option<i32>,
    pub screenshot_thumb_dark_artifact_id: Option<i32>,
    pub screenshot_error_artifact_id: Option<i32>,
}

impl SnapshotFetcher {
    pub fn new(job_id: i32) -> Self {
        Self { job_id }
    }
}

impl Processor for SnapshotFetcher {
    type Output = SnapshotFetchOutput;

    async fn process<'a>(
        &'a mut self,
        hyperlink: &'a mut hyperlink::ActiveModel,
        connection: &'a DatabaseConnection,
    ) -> Result<Self::Output, super::processor::ProcessingError> {
        let hyperlink_id = *hyperlink.id.as_ref();
        let source_url = hyperlink.url.as_ref().to_string();
        let collection_settings = settings::load(connection)
            .await
            .map_err(ProcessingError::DB)?;

        if hyperlink_source_type(hyperlink) == HyperlinkSourceType::Pdf {
            if let Some(source_artifact) = hyperlink_artifact::latest_for_hyperlink_kind(
                connection,
                hyperlink_id,
                HyperlinkArtifactKind::PdfSource,
            )
            .await
            .map_err(ProcessingError::DB)?
            {
                let mut output = SnapshotFetchOutput {
                    source_artifact_id: source_artifact.id,
                    source_artifact_kind: HyperlinkArtifactKind::PdfSource,
                    screenshot_artifact_id: None,
                    screenshot_dark_artifact_id: None,
                    screenshot_thumb_artifact_id: None,
                    screenshot_thumb_dark_artifact_id: None,
                    screenshot_error_artifact_id: None,
                };

                if collection_settings.collect_screenshots {
                    let pdf_payload = hyperlink_artifact::load_payload(&source_artifact)
                        .await
                        .map_err(ProcessingError::DB)?;
                    match build_pdf_thumbnails_from_source(
                        &pdf_payload,
                        screenshot_thumbnail_size(),
                    ) {
                        Ok((thumbnail_webp, thumbnail_dark_webp)) => {
                            let thumbnail_artifact = hyperlink_artifact::insert(
                                connection,
                                hyperlink_id,
                                Some(self.job_id),
                                HyperlinkArtifactKind::ScreenshotThumbWebp,
                                thumbnail_webp,
                                SCREENSHOT_CONTENT_TYPE,
                            )
                            .await
                            .map_err(ProcessingError::DB)?;
                            output.screenshot_thumb_artifact_id = Some(thumbnail_artifact.id);

                            let dark_thumbnail_artifact = hyperlink_artifact::insert(
                                connection,
                                hyperlink_id,
                                Some(self.job_id),
                                HyperlinkArtifactKind::ScreenshotThumbDarkWebp,
                                thumbnail_dark_webp,
                                SCREENSHOT_CONTENT_TYPE,
                            )
                            .await
                            .map_err(ProcessingError::DB)?;
                            output.screenshot_thumb_dark_artifact_id =
                                Some(dark_thumbnail_artifact.id);
                        }
                        Err(error) => {
                            let payload = encode_screenshot_failure_payload(
                                &source_url,
                                vec![format!("failed to render pdf thumbnail: {error}")],
                                None,
                                "error",
                            );
                            let error_artifact = hyperlink_artifact::insert(
                                connection,
                                hyperlink_id,
                                Some(self.job_id),
                                HyperlinkArtifactKind::ScreenshotError,
                                payload,
                                SCREENSHOT_ERROR_CONTENT_TYPE,
                            )
                            .await
                            .map_err(ProcessingError::DB)?;
                            output.screenshot_error_artifact_id = Some(error_artifact.id);
                        }
                    }
                }

                hyperlink.source_type = sea_orm::ActiveValue::Set(HyperlinkSourceType::Pdf);
                return Ok(output);
            }

            if !is_absolute_http_or_https_url(&source_url) {
                return Err(ProcessingError::FetchError(
                    "pdf_source artifact is missing and hyperlink URL is not an absolute http/https URL; fetch PDF Source first.".to_string(),
                ));
            }
        }

        match capture_snapshot(hyperlink.url.as_ref()).await {
            Ok(capture) => {
                let mut pdf_source_payload_for_thumbnail = None;
                let (kind, payload, content_type) = match capture {
                    SnapshotCapture::Html { archive } => {
                        let compressed =
                            hyperlink_artifact::compress_snapshot_warc_payload(&archive)
                                .map_err(ProcessingError::DB)?;
                        (
                            HyperlinkArtifactKind::SnapshotWarc,
                            compressed,
                            SNAPSHOT_CONTENT_TYPE.to_string(),
                        )
                    }
                    SnapshotCapture::Pdf {
                        payload,
                        content_type,
                    } => {
                        pdf_source_payload_for_thumbnail = Some(payload.clone());
                        (HyperlinkArtifactKind::PdfSource, payload, content_type)
                    }
                };

                hyperlink.source_type = sea_orm::ActiveValue::Set(match kind {
                    HyperlinkArtifactKind::PdfSource => HyperlinkSourceType::Pdf,
                    HyperlinkArtifactKind::SnapshotWarc => HyperlinkSourceType::Html,
                    _ => HyperlinkSourceType::Unknown,
                });

                let source_artifact = hyperlink_artifact::insert(
                    connection,
                    hyperlink_id,
                    Some(self.job_id),
                    kind.clone(),
                    payload,
                    &content_type,
                )
                .await
                .map_err(ProcessingError::DB)?;

                let mut output = SnapshotFetchOutput {
                    source_artifact_id: source_artifact.id,
                    source_artifact_kind: kind,
                    screenshot_artifact_id: None,
                    screenshot_dark_artifact_id: None,
                    screenshot_thumb_artifact_id: None,
                    screenshot_thumb_dark_artifact_id: None,
                    screenshot_error_artifact_id: None,
                };

                if !collection_settings.collect_screenshots {
                    return Ok(output);
                }

                if should_skip_screenshot_capture_for_source(&output.source_artifact_kind) {
                    if let Some(pdf_payload) = pdf_source_payload_for_thumbnail.as_deref() {
                        match build_pdf_thumbnails_from_source(
                            pdf_payload,
                            screenshot_thumbnail_size(),
                        ) {
                            Ok((thumbnail_webp, thumbnail_dark_webp)) => {
                                let thumbnail_artifact = hyperlink_artifact::insert(
                                    connection,
                                    hyperlink_id,
                                    Some(self.job_id),
                                    HyperlinkArtifactKind::ScreenshotThumbWebp,
                                    thumbnail_webp,
                                    SCREENSHOT_CONTENT_TYPE,
                                )
                                .await
                                .map_err(ProcessingError::DB)?;
                                output.screenshot_thumb_artifact_id = Some(thumbnail_artifact.id);

                                let dark_thumbnail_artifact = hyperlink_artifact::insert(
                                    connection,
                                    hyperlink_id,
                                    Some(self.job_id),
                                    HyperlinkArtifactKind::ScreenshotThumbDarkWebp,
                                    thumbnail_dark_webp,
                                    SCREENSHOT_CONTENT_TYPE,
                                )
                                .await
                                .map_err(ProcessingError::DB)?;
                                output.screenshot_thumb_dark_artifact_id =
                                    Some(dark_thumbnail_artifact.id);
                            }
                            Err(error) => {
                                let payload = encode_screenshot_failure_payload(
                                    &source_url,
                                    vec![format!("failed to render pdf thumbnail: {error}")],
                                    None,
                                    "error",
                                );
                                let error_artifact = hyperlink_artifact::insert(
                                    connection,
                                    hyperlink_id,
                                    Some(self.job_id),
                                    HyperlinkArtifactKind::ScreenshotError,
                                    payload,
                                    SCREENSHOT_ERROR_CONTENT_TYPE,
                                )
                                .await
                                .map_err(ProcessingError::DB)?;
                                output.screenshot_error_artifact_id = Some(error_artifact.id);
                            }
                        }
                    }
                    return Ok(output);
                }

                match capture_screenshots(&source_url, collection_settings.collect_screenshot_dark)
                    .await
                {
                    Ok(capture) => {
                        let ScreenshotCapture {
                            desktop_webp,
                            desktop_dark_webp,
                            thumbnail_webp,
                            thumbnail_dark_webp,
                            warnings,
                            attempts,
                        } = capture;
                        let screenshot_artifact = hyperlink_artifact::insert(
                            connection,
                            hyperlink_id,
                            Some(self.job_id),
                            HyperlinkArtifactKind::ScreenshotWebp,
                            desktop_webp,
                            SCREENSHOT_CONTENT_TYPE,
                        )
                        .await
                        .map_err(ProcessingError::DB)?;
                        output.screenshot_artifact_id = Some(screenshot_artifact.id);

                        if let Some(dark_webp) = desktop_dark_webp {
                            let dark_artifact = hyperlink_artifact::insert(
                                connection,
                                hyperlink_id,
                                Some(self.job_id),
                                HyperlinkArtifactKind::ScreenshotDarkWebp,
                                dark_webp,
                                SCREENSHOT_CONTENT_TYPE,
                            )
                            .await
                            .map_err(ProcessingError::DB)?;
                            output.screenshot_dark_artifact_id = Some(dark_artifact.id);
                        }

                        let thumbnail_artifact = hyperlink_artifact::insert(
                            connection,
                            hyperlink_id,
                            Some(self.job_id),
                            HyperlinkArtifactKind::ScreenshotThumbWebp,
                            thumbnail_webp,
                            SCREENSHOT_CONTENT_TYPE,
                        )
                        .await
                        .map_err(ProcessingError::DB)?;
                        output.screenshot_thumb_artifact_id = Some(thumbnail_artifact.id);

                        if let Some(dark_thumbnail_webp) = thumbnail_dark_webp {
                            let dark_thumbnail_artifact = hyperlink_artifact::insert(
                                connection,
                                hyperlink_id,
                                Some(self.job_id),
                                HyperlinkArtifactKind::ScreenshotThumbDarkWebp,
                                dark_thumbnail_webp,
                                SCREENSHOT_CONTENT_TYPE,
                            )
                            .await
                            .map_err(ProcessingError::DB)?;
                            output.screenshot_thumb_dark_artifact_id =
                                Some(dark_thumbnail_artifact.id);
                        }

                        if !warnings.is_empty() {
                            let attempts = (!attempts.is_empty()).then_some(attempts);
                            let payload = encode_screenshot_failure_payload(
                                &source_url,
                                warnings,
                                attempts,
                                "warning",
                            );
                            let warning_artifact = hyperlink_artifact::insert(
                                connection,
                                hyperlink_id,
                                Some(self.job_id),
                                HyperlinkArtifactKind::ScreenshotError,
                                payload,
                                SCREENSHOT_ERROR_CONTENT_TYPE,
                            )
                            .await
                            .map_err(ProcessingError::DB)?;
                            output.screenshot_error_artifact_id = Some(warning_artifact.id);
                        }
                    }
                    Err(error) => {
                        let attempts = (!error.attempts.is_empty()).then_some(error.attempts);
                        let payload = encode_screenshot_failure_payload(
                            &source_url,
                            vec![error.message],
                            attempts,
                            "error",
                        );

                        let error_artifact = hyperlink_artifact::insert(
                            connection,
                            hyperlink_id,
                            Some(self.job_id),
                            HyperlinkArtifactKind::ScreenshotError,
                            payload,
                            SCREENSHOT_ERROR_CONTENT_TYPE,
                        )
                        .await
                        .map_err(ProcessingError::DB)?;
                        output.screenshot_error_artifact_id = Some(error_artifact.id);
                    }
                }

                Ok(output)
            }
            Err(error) => {
                let payload = serde_json::to_vec_pretty(&error.artifact)
                    .unwrap_or_else(|encode_error| {
                        format!(
                            "{{\"error\":\"failed to encode snapshot error payload: {encode_error}\"}}"
                        )
                        .into_bytes()
                    });
                hyperlink_artifact::insert(
                    connection,
                    hyperlink_id,
                    Some(self.job_id),
                    HyperlinkArtifactKind::SnapshotError,
                    payload,
                    SNAPSHOT_ERROR_CONTENT_TYPE,
                )
                .await
                .map_err(ProcessingError::DB)?;
                Err(ProcessingError::FetchError(error.message))
            }
        }
    }
}

fn should_skip_screenshot_capture_for_source(source_kind: &HyperlinkArtifactKind) -> bool {
    matches!(source_kind, HyperlinkArtifactKind::PdfSource)
}

fn hyperlink_source_type(hyperlink: &hyperlink::ActiveModel) -> HyperlinkSourceType {
    match &hyperlink.source_type {
        sea_orm::ActiveValue::Set(value) | sea_orm::ActiveValue::Unchanged(value) => value.clone(),
        sea_orm::ActiveValue::NotSet => HyperlinkSourceType::Unknown,
    }
}

fn is_absolute_http_or_https_url(value: &str) -> bool {
    Url::parse(value)
        .ok()
        .is_some_and(|url| matches!(url.scheme(), "http" | "https"))
}

enum SnapshotCapture {
    Html {
        archive: Vec<u8>,
    },
    Pdf {
        payload: Vec<u8>,
        content_type: String,
    },
}

struct SnapshotCaptureError {
    message: String,
    artifact: SnapshotFailureArtifact,
}

struct ScreenshotCapture {
    desktop_webp: Vec<u8>,
    desktop_dark_webp: Option<Vec<u8>>,
    thumbnail_webp: Vec<u8>,
    thumbnail_dark_webp: Option<Vec<u8>>,
    warnings: Vec<String>,
    attempts: Vec<ScreenshotCaptureAttempt>,
}

#[derive(Debug)]
struct SingleScreenshotCapture {
    bytes: Vec<u8>,
    warning: Option<String>,
    attempts: Vec<ScreenshotCaptureAttempt>,
}

#[derive(Debug)]
struct ScreenshotCaptureFailure {
    message: String,
    attempts: Vec<ScreenshotCaptureAttempt>,
}

#[derive(Clone, Copy, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ScreenshotCaptureStage {
    ExactHeight,
    FixedViewport,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
struct ScreenshotCaptureAttempt {
    attempt: usize,
    stage: ScreenshotCaptureStage,
    error: String,
    retryable: bool,
}

#[derive(Clone, Copy)]
struct Viewport {
    width: u32,
    height: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PageHeightBounds {
    min: u32,
    max: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExactHeightCaptureState {
    enabled_for_job: bool,
}

impl ExactHeightCaptureState {
    fn from_enabled(enabled: bool) -> Self {
        Self {
            enabled_for_job: enabled,
        }
    }

    fn should_try_exact_height(self) -> bool {
        self.enabled_for_job
    }

    fn disable_for_job_on_error(&mut self, error: &str) -> bool {
        if self.enabled_for_job && is_exact_height_empty_payload_error(error) {
            self.enabled_for_job = false;
            return true;
        }
        false
    }
}

#[async_trait::async_trait]
trait BrowserLifecycle {
    async fn close_browser(&mut self) -> Result<(), String>;
    async fn wait_for_exit(&mut self) -> Result<Option<()>, String>;
    async fn kill_browser(&mut self) -> Result<bool, String>;
}

struct ChromiumBrowserLifecycle<'a> {
    browser: &'a mut Browser,
}

#[async_trait::async_trait]
impl BrowserLifecycle for ChromiumBrowserLifecycle<'_> {
    async fn close_browser(&mut self) -> Result<(), String> {
        self.browser
            .close()
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    async fn wait_for_exit(&mut self) -> Result<Option<()>, String> {
        self.browser
            .wait()
            .await
            .map(|status| status.map(|_| ()))
            .map_err(|error| error.to_string())
    }

    async fn kill_browser(&mut self) -> Result<bool, String> {
        match self.browser.kill().await {
            Some(Ok(())) => Ok(true),
            Some(Err(error)) => Err(error.to_string()),
            None => Ok(false),
        }
    }
}

struct ChromiumoxideSession {
    browser: Browser,
    handler_task: tokio::task::JoinHandle<()>,
    profile_dir: PathBuf,
}

impl ChromiumoxideSession {
    async fn launch(
        window_size: Viewport,
        timeout: Duration,
        variant: Option<ScreenshotVariant>,
        render_wait_ms: u64,
    ) -> Result<Self, String> {
        maybe_sweep_stale_screenshot_profiles().await;
        let profile_dir = screenshot_profile_dir();
        tokio::fs::create_dir_all(&profile_dir)
            .await
            .map_err(|err| {
                format!(
                    "failed to create temporary chromium profile {}: {err}",
                    profile_dir.display()
                )
            })?;
        let profile_slug = profile_dir
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "profile".to_string());
        let runtime_dir = chromium_runtime_dir().join(profile_slug);
        ensure_chromium_runtime_dir_at(&runtime_dir).await?;

        let config = BrowserConfig::builder()
            .new_headless_mode()
            .chrome_executable(chromium_path())
            .window_size(window_size.width, window_size.height.max(1))
            .request_timeout(timeout)
            .user_data_dir(profile_dir.clone())
            .env("XDG_RUNTIME_DIR", runtime_dir.to_string_lossy().to_string())
            .args(chromium_launch_args(variant, render_wait_ms))
            .no_sandbox()
            .build()
            .map_err(|err| format!("failed to build chromiumoxide browser config: {err}"))?;

        let (browser, mut handler) = Browser::launch(config)
            .await
            .map_err(|err| format!("failed to launch chromium browser: {err}"))?;

        let handler_task = tokio::spawn(async move {
            while let Some(event) = handler.next().await {
                if let Err(error) = event {
                    tracing::warn!(error = %error, "chromiumoxide handler error");
                    break;
                }
            }
        });

        Ok(Self {
            browser,
            handler_task,
            profile_dir,
        })
    }

    async fn shutdown(mut self) -> Vec<String> {
        let mut lifecycle = ChromiumBrowserLifecycle {
            browser: &mut self.browser,
        };
        let mut warnings = shutdown_browser_lifecycle(&mut lifecycle, &self.profile_dir).await;
        self.handler_task.abort();

        sleep(Duration::from_millis(
            SCREENSHOT_PROFILE_CLEANUP_RETRY_BASE_DELAY_MS,
        ))
        .await;
        if let Err(error) = remove_directory_with_retries(
            &self.profile_dir,
            SCREENSHOT_PROFILE_CLEANUP_RETRY_ATTEMPTS,
            SCREENSHOT_PROFILE_CLEANUP_RETRY_BASE_DELAY_MS,
        )
        .await
        {
            warnings.push(format!(
                "failed to clean up temporary chromium profile {}: {error}",
                self.profile_dir.display()
            ));
        }

        warnings
    }
}

async fn wait_for_browser_exit_with_timeout<L: BrowserLifecycle>(
    lifecycle: &mut L,
    stage: &str,
    warnings: &mut Vec<String>,
) -> bool {
    match tokio::time::timeout(CHROMIUM_SHUTDOWN_WAIT_TIMEOUT, lifecycle.wait_for_exit()).await {
        Ok(Ok(Some(()))) => true,
        Ok(Ok(None)) => true,
        Ok(Err(error)) => {
            warnings.push(format!(
                "failed while waiting for chromium browser process to exit after {stage}: {error}"
            ));
            false
        }
        Err(_) => {
            warnings.push(format!(
                "timed out waiting for chromium browser process to exit after {stage}"
            ));
            false
        }
    }
}

async fn shutdown_browser_lifecycle<L: BrowserLifecycle>(
    lifecycle: &mut L,
    profile_dir: &Path,
) -> Vec<String> {
    let mut warnings = Vec::new();
    let close_ok = match lifecycle.close_browser().await {
        Ok(()) => true,
        Err(error) => {
            warnings.push(format!("failed to close chromium browser: {error}"));
            false
        }
    };

    let mut shutdown_reaped =
        wait_for_browser_exit_with_timeout(lifecycle, "close", &mut warnings).await;
    let mut kill_invoked = false;
    if !shutdown_reaped {
        kill_invoked = true;
        match lifecycle.kill_browser().await {
            Ok(true) => {}
            Ok(false) => warnings.push(
                "chromium browser kill was skipped because no child process handle was available"
                    .to_string(),
            ),
            Err(error) => {
                warnings.push(format!("failed to kill chromium browser process: {error}"))
            }
        }

        if wait_for_browser_exit_with_timeout(lifecycle, "kill", &mut warnings).await {
            shutdown_reaped = true;
        }
    }

    let shutdown_path = if shutdown_reaped {
        if kill_invoked {
            "kill_wait"
        } else {
            "close_wait"
        }
    } else {
        "drop_fallback"
    };
    tracing::info!(
        close_ok,
        kill_invoked,
        shutdown_reaped,
        shutdown_path,
        profile_dir = %profile_dir.display(),
        "chromium browser shutdown result"
    );
    if !shutdown_reaped {
        tracing::error!(
            profile_dir = %profile_dir.display(),
            close_ok,
            kill_invoked,
            shutdown_reaped,
            shutdown_path,
            "chromium browser process was not confirmed as reaped"
        );
        warnings.push(
            "chromium browser process was not confirmed as reaped; relying on runtime kill_on_drop fallback"
                .to_string(),
        );
    }

    warnings
}

#[derive(Clone)]
struct FetchedResponse {
    url: Url,
    status: u16,
    content_type: Option<String>,
    body: Vec<u8>,
    truncated: bool,
}

#[derive(Clone)]
struct FetchAttemptError {
    message: String,
    retryable: bool,
    status: Option<u16>,
}

struct RetryFailure {
    attempts: Vec<SnapshotFetchAttempt>,
    final_error: FetchAttemptError,
}

#[derive(Clone, Serialize)]
struct SnapshotFetchAttempt {
    attempt: usize,
    url: String,
    elapsed_ms: u128,
    error: String,
    retryable: bool,
    status: Option<u16>,
}

#[derive(Serialize)]
struct SnapshotFailureArtifact {
    source_url: String,
    failed_at: String,
    stage: String,
    final_error: String,
    attempts: Vec<SnapshotFetchAttempt>,
    retry_policy: SnapshotRetryPolicy,
}

#[derive(Serialize)]
struct ScreenshotFailureArtifact {
    source_url: String,
    failed_at: String,
    errors: Vec<String>,
    chromium_path: String,
    timeout_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    attempts: Option<Vec<ScreenshotCaptureAttempt>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    font_diagnostics: Option<ScreenshotFontDiagnostics>,
}

#[derive(Clone, Copy)]
enum ScreenshotVariant {
    Light,
    Dark,
}

#[derive(Serialize)]
struct SnapshotManifest {
    source_url: String,
    captured_at: String,
    capture_method: String,
    fallback_used: bool,
    chromium_error: Option<String>,
    html: SnapshotAsset,
    css: Vec<SnapshotAsset>,
    css_errors: Vec<SnapshotError>,
    limits: SnapshotLimits,
    retry_policy: SnapshotRetryPolicy,
}

#[derive(Serialize)]
struct SnapshotAsset {
    url: String,
    status: u16,
    content_type: Option<String>,
    bytes: usize,
    truncated: bool,
}

#[derive(Serialize)]
struct SnapshotError {
    url: String,
    error: String,
}

#[derive(Serialize)]
struct SnapshotLimits {
    max_html_bytes: usize,
    max_css_files: usize,
    max_css_bytes_per_file: usize,
    max_total_css_bytes: usize,
}

#[derive(Serialize)]
struct SnapshotRetryPolicy {
    max_attempts: usize,
    request_timeout_secs: u64,
    snapshot_deadline_secs: u64,
    backoff_base_ms: u64,
    jitter_max_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SnapshotSourceKind {
    Html,
    Pdf,
    Unsupported,
}

#[derive(Clone, Copy)]
enum HtmlCaptureMethod {
    Chromium,
    Reqwest,
    ReqwestFallback,
}

impl HtmlCaptureMethod {
    fn as_str(self) -> &'static str {
        match self {
            Self::Chromium => "chromium",
            Self::Reqwest => "reqwest",
            Self::ReqwestFallback => "reqwest_fallback",
        }
    }
}

async fn capture_snapshot(url: &str) -> Result<SnapshotCapture, SnapshotCaptureError> {
    let parsed = Url::parse(url).map_err(|err| {
        snapshot_capture_error(
            url,
            "input_validation",
            format!("invalid url: {err}"),
            Vec::new(),
        )
    })?;

    ensure_fetchable_url(&parsed).await.map_err(|err| {
        snapshot_capture_error(parsed.as_str(), "input_validation", err, Vec::new())
    })?;

    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|err| {
            snapshot_capture_error(
                parsed.as_str(),
                "client_init",
                format!("failed to build http client: {err}"),
                Vec::new(),
            )
        })?;

    let deadline = Instant::now() + SNAPSHOT_DEADLINE;
    if is_likely_pdf_url(&parsed) {
        let source_response =
            fetch_response_with_retry(&client, parsed.clone(), MAX_HTML_BYTES, deadline)
                .await
                .map_err(|failure| {
                    snapshot_capture_error(
                        parsed.as_str(),
                        "html_fetch",
                        failure.final_error.message,
                        failure.attempts,
                    )
                })?;
        return capture_snapshot_from_source_response(
            &client,
            source_response,
            deadline,
            HtmlCaptureMethod::Reqwest,
            false,
            None,
        )
        .await;
    }

    match capture_html_with_chromium_response(parsed.clone(), deadline).await {
        Ok(chromium_response) => {
            if looks_like_pdf_viewer_dom(&chromium_response.body) {
                let source_response =
                    fetch_response_with_retry(&client, parsed.clone(), MAX_HTML_BYTES, deadline)
                        .await
                        .map_err(|failure| {
                            snapshot_capture_error(
                                parsed.as_str(),
                                "reqwest_fallback",
                                format!(
                                    "chromium dump-dom looked like a PDF viewer; reqwest source capture failed: {}",
                                    failure.final_error.message
                                ),
                                failure.attempts,
                            )
                        })?;
                return capture_snapshot_from_source_response(
                    &client,
                    source_response,
                    deadline,
                    HtmlCaptureMethod::ReqwestFallback,
                    true,
                    Some(
                        "chromium dump-dom looked like a PDF viewer; used reqwest source capture"
                            .to_string(),
                    ),
                )
                .await;
            }

            capture_snapshot_from_source_response(
                &client,
                chromium_response,
                deadline,
                HtmlCaptureMethod::Chromium,
                false,
                None,
            )
            .await
        }
        Err(chromium_error) => {
            let source_response =
                fetch_response_with_retry(&client, parsed.clone(), MAX_HTML_BYTES, deadline)
                    .await
                    .map_err(|failure| {
                        snapshot_capture_error(
                            parsed.as_str(),
                            "chromium_fallback",
                            format!(
                                "chromium content capture failed: {chromium_error}; reqwest source capture failed: {}",
                                failure.final_error.message
                            ),
                            failure.attempts,
                        )
                    })?;

            capture_snapshot_from_source_response(
                &client,
                source_response,
                deadline,
                HtmlCaptureMethod::ReqwestFallback,
                true,
                Some(chromium_error),
            )
            .await
        }
    }
}

async fn capture_snapshot_from_source_response(
    client: &reqwest::Client,
    source_response: FetchedResponse,
    deadline: Instant,
    capture_method: HtmlCaptureMethod,
    fallback_used: bool,
    chromium_error: Option<String>,
) -> Result<SnapshotCapture, SnapshotCaptureError> {
    let source_kind = classify_source_kind(
        source_response.content_type.as_deref(),
        &source_response.body,
    );
    if matches!(source_kind, SnapshotSourceKind::Pdf) {
        return Ok(SnapshotCapture::Pdf {
            payload: source_response.body,
            content_type: source_response
                .content_type
                .unwrap_or_else(|| PDF_SOURCE_DEFAULT_CONTENT_TYPE.to_string()),
        });
    }
    if matches!(source_kind, SnapshotSourceKind::Unsupported) {
        return Err(snapshot_capture_error(
            source_response.url.as_str(),
            "source_validation",
            "snapshot source is neither HTML nor PDF".to_string(),
            Vec::new(),
        ));
    }

    capture_snapshot_from_html_response(
        client,
        source_response,
        deadline,
        capture_method,
        fallback_used,
        chromium_error,
    )
    .await
}

async fn capture_snapshot_from_html_response(
    client: &reqwest::Client,
    source_response: FetchedResponse,
    deadline: Instant,
    capture_method: HtmlCaptureMethod,
    fallback_used: bool,
    chromium_error: Option<String>,
) -> Result<SnapshotCapture, SnapshotCaptureError> {
    let source_url = source_response.url.clone();
    let html_text = String::from_utf8_lossy(&source_response.body);
    let stylesheets = extract_stylesheet_hrefs(&html_text);

    let mut css_assets = Vec::new();
    let mut css_errors = Vec::new();
    let mut seen = HashSet::new();
    let mut total_css_bytes = 0usize;

    for href in stylesheets {
        if Instant::now() >= deadline {
            css_errors.push(SnapshotError {
                url: href,
                error: "skipped: snapshot deadline reached".to_string(),
            });
            continue;
        }

        if css_assets.len() >= MAX_CSS_FILES {
            css_errors.push(SnapshotError {
                url: href,
                error: format!("skipped: stylesheet limit ({MAX_CSS_FILES}) reached"),
            });
            continue;
        }

        if total_css_bytes >= MAX_TOTAL_CSS_BYTES {
            css_errors.push(SnapshotError {
                url: href,
                error: format!("skipped: total css byte limit ({MAX_TOTAL_CSS_BYTES}) reached"),
            });
            continue;
        }

        let resolved = match source_url.join(href.trim()) {
            Ok(url) => url,
            Err(err) => {
                css_errors.push(SnapshotError {
                    url: href,
                    error: format!("invalid stylesheet url: {err}"),
                });
                continue;
            }
        };

        let dedupe_key = resolved.as_str().to_string();
        if !seen.insert(dedupe_key) {
            continue;
        }

        let remaining = MAX_TOTAL_CSS_BYTES - total_css_bytes;
        let file_limit = remaining.min(MAX_CSS_BYTES_PER_FILE);
        match fetch_response_with_retry(client, resolved, file_limit, deadline).await {
            Ok(css_response) => {
                total_css_bytes += css_response.body.len();
                css_assets.push(css_response);
            }
            Err(error) => {
                css_errors.push(SnapshotError {
                    url: href,
                    error: format_retry_failure(&error),
                });
            }
        }
    }

    let manifest = SnapshotManifest {
        source_url: source_url.to_string(),
        captured_at: now_utc().to_string(),
        capture_method: capture_method.as_str().to_string(),
        fallback_used,
        chromium_error,
        html: SnapshotAsset {
            url: source_response.url.to_string(),
            status: source_response.status,
            content_type: source_response.content_type.clone(),
            bytes: source_response.body.len(),
            truncated: source_response.truncated,
        },
        css: css_assets
            .iter()
            .map(|asset| SnapshotAsset {
                url: asset.url.to_string(),
                status: asset.status,
                content_type: asset.content_type.clone(),
                bytes: asset.body.len(),
                truncated: asset.truncated,
            })
            .collect(),
        css_errors,
        limits: SnapshotLimits {
            max_html_bytes: MAX_HTML_BYTES,
            max_css_files: MAX_CSS_FILES,
            max_css_bytes_per_file: MAX_CSS_BYTES_PER_FILE,
            max_total_css_bytes: MAX_TOTAL_CSS_BYTES,
        },
        retry_policy: retry_policy(),
    };

    let mut archive = Vec::new();
    append_record(
        &mut archive,
        "response",
        source_response.url.as_str(),
        source_response
            .content_type
            .as_deref()
            .unwrap_or("application/octet-stream"),
        &source_response.body,
        &[
            ("X-HTTP-Status", source_response.status.to_string()),
            ("X-Truncated", source_response.truncated.to_string()),
        ],
    );

    for css in css_assets {
        append_record(
            &mut archive,
            "response",
            css.url.as_str(),
            css.content_type
                .as_deref()
                .unwrap_or("application/octet-stream"),
            &css.body,
            &[
                ("X-HTTP-Status", css.status.to_string()),
                ("X-Truncated", css.truncated.to_string()),
            ],
        );
    }

    let manifest_payload = serde_json::to_vec_pretty(&manifest).map_err(|err| {
        snapshot_capture_error(
            source_url.as_str(),
            "manifest_encode",
            format!("manifest encode failed: {err}"),
            Vec::new(),
        )
    })?;
    append_record(
        &mut archive,
        "metadata",
        source_url.as_str(),
        "application/json",
        &manifest_payload,
        &[],
    );

    Ok(SnapshotCapture::Html { archive })
}

async fn capture_html_with_chromium_response(
    url: Url,
    deadline: Instant,
) -> Result<FetchedResponse, String> {
    if Instant::now() >= deadline {
        return Err("snapshot deadline reached before chromium content capture".to_string());
    }

    let timeout =
        snapshot_content_timeout().min(deadline.saturating_duration_since(Instant::now()));
    if timeout.is_zero() {
        return Err("snapshot deadline reached before chromium content capture".to_string());
    }

    let session = ChromiumoxideSession::launch(
        content_capture_viewport(),
        timeout,
        None,
        snapshot_content_render_wait_ms(),
    )
    .await?;
    let capture_result = tokio::time::timeout(timeout, async {
        let page = session
            .browser
            .new_page("about:blank")
            .await
            .map_err(|err| format!("failed to create chromium page for content capture: {err}"))?;

        page.goto(url.as_str())
            .await
            .map_err(|err| format!("chromium content capture navigation failed: {err}"))?;
        sleep(Duration::from_millis(snapshot_content_render_wait_ms())).await;

        let body = page
            .content_bytes()
            .await
            .map_err(|err| format!("chromium content capture failed to read page html: {err}"))?;
        if body.is_empty() {
            return Err("chromium content capture produced an empty DOM".to_string());
        }

        Ok::<Vec<u8>, String>(body.to_vec())
    })
    .await
    .map_err(|_| {
        format!(
            "chromium content capture timed out after {}s",
            timeout.as_secs()
        )
    })?;

    let shutdown_warnings = session.shutdown().await;
    log_chromium_shutdown_warnings(&shutdown_warnings);
    let mut body = capture_result?;

    let mut truncated = false;
    if body.len() > MAX_HTML_BYTES {
        body.truncate(MAX_HTML_BYTES);
        truncated = true;
    }

    Ok(FetchedResponse {
        url,
        status: 200,
        content_type: Some("text/html; charset=utf-8".to_string()),
        body,
        truncated,
    })
}

async fn capture_screenshots(
    url: &str,
    collect_dark_variant: bool,
) -> Result<ScreenshotCapture, ScreenshotCaptureFailure> {
    let mut warnings = Vec::new();
    let mut attempts = Vec::new();
    let mut exact_height_state =
        ExactHeightCaptureState::from_enabled(screenshot_exact_height_enabled());
    let parsed = Url::parse(url).map_err(|err| ScreenshotCaptureFailure {
        message: format!("invalid screenshot url: {err}"),
        attempts: Vec::new(),
    })?;
    ensure_fetchable_url(&parsed)
        .await
        .map_err(|message| ScreenshotCaptureFailure {
            message,
            attempts: Vec::new(),
        })?;
    let desktop_viewport = screenshot_desktop_viewport();
    let desktop_capture = capture_single_screenshot(
        parsed.as_str(),
        desktop_viewport,
        ScreenshotVariant::Light,
        &mut exact_height_state,
    )
    .await?;
    let SingleScreenshotCapture {
        bytes: desktop_webp,
        warning: desktop_warning,
        attempts: desktop_attempts,
    } = desktop_capture;
    attempts.extend(desktop_attempts);
    if let Some(warning) = desktop_warning {
        warnings.push(format!("light screenshot fallback: {warning}"));
    }
    let thumbnail_webp = match build_square_thumbnail(&desktop_webp, screenshot_thumbnail_size()) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Err(ScreenshotCaptureFailure {
                message: error,
                attempts,
            });
        }
    };

    let (desktop_dark_webp, thumbnail_dark_webp) = if collect_dark_variant
        && screenshot_dark_mode_enabled()
    {
        match capture_single_screenshot(
            parsed.as_str(),
            desktop_viewport,
            ScreenshotVariant::Dark,
            &mut exact_height_state,
        )
        .await
        {
            Ok(capture) => {
                let SingleScreenshotCapture {
                    bytes,
                    warning,
                    attempts: dark_attempts,
                } = capture;
                attempts.extend(dark_attempts);
                if let Some(warning) = warning {
                    warnings.push(format!("dark screenshot fallback: {warning}"));
                }
                let thumbnail = match build_square_thumbnail(&bytes, screenshot_thumbnail_size()) {
                    Ok(thumbnail) => Some(thumbnail),
                    Err(error) => {
                        warnings.push(format!("dark thumbnail build failed: {error}"));
                        None
                    }
                };
                (Some(bytes), thumbnail)
            }
            Err(error) => {
                warnings.push(format!("dark screenshot failed: {}", error.message));
                attempts.extend(error.attempts);
                (None, None)
            }
        }
    } else {
        (None, None)
    };

    Ok(ScreenshotCapture {
        desktop_webp,
        desktop_dark_webp,
        thumbnail_webp,
        thumbnail_dark_webp,
        warnings,
        attempts,
    })
}

async fn capture_single_screenshot(
    url: &str,
    viewport: Viewport,
    variant: ScreenshotVariant,
    exact_height_state: &mut ExactHeightCaptureState,
) -> Result<SingleScreenshotCapture, ScreenshotCaptureFailure> {
    if !exact_height_state.should_try_exact_height() {
        return capture_single_screenshot_fixed_viewport_with_retries(url, viewport, variant).await;
    }

    match capture_single_screenshot_exact_height_with_retries(url, viewport, variant).await {
        Ok(capture) => Ok(capture),
        Err(exact_error) => {
            let exact_message = exact_error.message;
            let mut attempts = exact_error.attempts;
            if should_skip_fixed_viewport_fallback_for_exact_error(&exact_message) {
                return Err(ScreenshotCaptureFailure {
                    message: format!(
                        "exact-height screenshot capture failed with non-recoverable chromium startup error: {exact_message}"
                    ),
                    attempts,
                });
            }
            let disabled_for_job = exact_height_state.disable_for_job_on_error(&exact_message);
            match capture_single_screenshot_fixed_viewport_with_retries(url, viewport, variant)
                .await
            {
                Ok(fallback_capture) => {
                    attempts.extend(fallback_capture.attempts);
                    let warning = if disabled_for_job {
                        format!(
                            "exact-height capture returned an empty payload and was disabled for this capture job; fixed-viewport fallback was used: {exact_message}"
                        )
                    } else {
                        format!(
                            "exact-height capture failed and fixed-viewport fallback was used: {exact_message}"
                        )
                    };
                    Ok(SingleScreenshotCapture {
                        bytes: fallback_capture.bytes,
                        warning: Some(warning),
                        attempts,
                    })
                }
                Err(fallback_error) => {
                    attempts.extend(fallback_error.attempts);
                    Err(ScreenshotCaptureFailure {
                        message: format!(
                            "exact-height screenshot capture failed: {exact_message}; fixed-viewport fallback failed: {}",
                            fallback_error.message
                        ),
                        attempts,
                    })
                }
            }
        }
    }
}

async fn capture_single_screenshot_exact_height_with_retries(
    url: &str,
    viewport: Viewport,
    variant: ScreenshotVariant,
) -> Result<SingleScreenshotCapture, ScreenshotCaptureFailure> {
    let max_attempts = screenshot_exact_height_attempts();
    let mut attempts = Vec::new();

    for attempt in 1..=max_attempts {
        match capture_single_screenshot_exact_height(url, viewport, variant).await {
            Ok(bytes) => {
                return Ok(SingleScreenshotCapture {
                    bytes,
                    warning: None,
                    attempts,
                });
            }
            Err(error) => {
                let retryable = exact_height_capture_error_retryable(&error);
                attempts.push(ScreenshotCaptureAttempt {
                    attempt,
                    stage: ScreenshotCaptureStage::ExactHeight,
                    error: error.clone(),
                    retryable,
                });

                if !retryable || attempt == max_attempts {
                    return Err(ScreenshotCaptureFailure {
                        message: error,
                        attempts,
                    });
                }

                sleep(screenshot_retry_backoff_delay(attempt)).await;
            }
        }
    }

    Err(ScreenshotCaptureFailure {
        message: "exact-height screenshot capture failed without an explicit attempt result"
            .to_string(),
        attempts,
    })
}

async fn capture_single_screenshot_fixed_viewport_with_retries(
    url: &str,
    viewport: Viewport,
    variant: ScreenshotVariant,
) -> Result<SingleScreenshotCapture, ScreenshotCaptureFailure> {
    let max_attempts = screenshot_fixed_viewport_attempts();
    let mut attempts = Vec::new();

    for attempt in 1..=max_attempts {
        match capture_single_screenshot_fixed_viewport(url, viewport, variant).await {
            Ok(bytes) => {
                return Ok(SingleScreenshotCapture {
                    bytes,
                    warning: None,
                    attempts,
                });
            }
            Err(error) => {
                let retryable = screenshot_capture_error_retryable(&error);
                attempts.push(ScreenshotCaptureAttempt {
                    attempt,
                    stage: ScreenshotCaptureStage::FixedViewport,
                    error: error.clone(),
                    retryable,
                });

                if !retryable || attempt == max_attempts {
                    return Err(ScreenshotCaptureFailure {
                        message: error,
                        attempts,
                    });
                }

                sleep(screenshot_retry_backoff_delay(attempt)).await;
            }
        }
    }

    Err(ScreenshotCaptureFailure {
        message: "fixed-viewport screenshot capture failed without an explicit attempt result"
            .to_string(),
        attempts,
    })
}

async fn capture_single_screenshot_exact_height(
    url: &str,
    viewport: Viewport,
    variant: ScreenshotVariant,
) -> Result<Vec<u8>, String> {
    let timeout = screenshot_timeout();
    tokio::time::timeout(
        timeout,
        capture_single_screenshot_exact_height_inner(url, viewport, variant),
    )
    .await
    .map_err(|_| {
        format!(
            "exact-height screenshot capture timed out after {}s",
            timeout.as_secs()
        )
    })?
}

async fn capture_single_screenshot_exact_height_inner(
    url: &str,
    viewport: Viewport,
    variant: ScreenshotVariant,
) -> Result<Vec<u8>, String> {
    let timeout = screenshot_timeout();
    let session = ChromiumoxideSession::launch(
        viewport,
        timeout,
        Some(variant),
        screenshot_render_wait_ms(),
    )
    .await?;
    let capture_result = tokio::time::timeout(timeout, async {
        let page = session
            .browser
            .new_page("about:blank")
            .await
            .map_err(|err| format!("failed to create chromium page for screenshot: {err}"))?;
        apply_screenshot_variant_media_emulation(&page, variant).await?;
        page.goto(url)
            .await
            .map_err(|err| format!("chromium screenshot navigation failed: {err}"))?;
        sleep(Duration::from_millis(screenshot_render_wait_ms())).await;

        let measured_height: f64 = page
            .evaluate_expression(page_height_expression())
            .await
            .map_err(|err| format!("failed to evaluate chromium page-height expression: {err}"))?
            .into_value()
            .map_err(|err| {
                format!("chromium page-height evaluation returned a non-numeric result: {err}")
            })?;
        let page_height = clamp_page_height(
            parse_page_height_from_value(measured_height)?,
            screenshot_page_height_bounds(),
        );
        let params = screenshot_params(true);
        let screenshot_bytes = page
            .execute(
                SetDeviceMetricsOverrideParams::builder()
                    .width(viewport.width)
                    .height(page_height)
                    .device_scale_factor(1.0)
                    .mobile(false)
                    .build()
                    .map_err(|err| {
                        format!("failed to build chromium metrics override command: {err}")
                    })?,
            )
            .await
            .map_err(|err| format!("failed to set chromium page-height metrics: {err}"))?;
        drop(screenshot_bytes);

        page.screenshot(params)
            .await
            .map_err(|err| format!("failed to capture chromium screenshot: {err}"))
    })
    .await
    .map_err(|_| {
        format!(
            "exact-height screenshot capture timed out after {}s",
            timeout.as_secs()
        )
    })?;

    let shutdown_warnings = session.shutdown().await;
    log_chromium_shutdown_warnings(&shutdown_warnings);
    let screenshot_bytes = capture_result?;
    if screenshot_bytes.is_empty() {
        return Err(EXACT_HEIGHT_EMPTY_PAYLOAD_ERROR.to_string());
    }
    Ok(screenshot_bytes)
}

fn page_height_expression() -> &'static str {
    "Math.max(document.documentElement?.scrollHeight || 0, document.body?.scrollHeight || 0, document.documentElement?.offsetHeight || 0, document.body?.offsetHeight || 0, document.documentElement?.clientHeight || 0)"
}

fn parse_page_height_from_value(height: f64) -> Result<u32, String> {
    if !height.is_finite() || height <= 0.0 {
        return Err(format!(
            "chromium page-height evaluation returned an invalid value: {height}"
        ));
    }

    let rounded = height.ceil();
    if rounded > u32::MAX as f64 {
        return Ok(u32::MAX);
    }
    Ok(rounded as u32)
}

fn screenshot_params(full_page: bool) -> ScreenshotParams {
    ScreenshotParams::builder()
        .format(CaptureScreenshotFormat::Webp)
        .quality(SCREENSHOT_CAPTURE_WEBP_QUALITY)
        .full_page(full_page)
        .build()
}

fn clamp_page_height(height: u32, bounds: PageHeightBounds) -> u32 {
    height.clamp(bounds.min, bounds.max)
}

fn screenshot_page_height_bounds() -> PageHeightBounds {
    let min = env_u64(
        "SCREENSHOT_MIN_PAGE_HEIGHT",
        DEFAULT_SCREENSHOT_MIN_PAGE_HEIGHT as u64,
        1,
        100_000,
    ) as u32;
    let max = env_u64(
        "SCREENSHOT_MAX_PAGE_HEIGHT",
        DEFAULT_SCREENSHOT_MAX_PAGE_HEIGHT as u64,
        1,
        100_000,
    ) as u32;
    normalize_page_height_bounds(PageHeightBounds { min, max })
}

fn normalize_page_height_bounds(bounds: PageHeightBounds) -> PageHeightBounds {
    if bounds.min <= bounds.max {
        bounds
    } else {
        PageHeightBounds {
            min: bounds.max,
            max: bounds.min,
        }
    }
}

fn screenshot_exact_height_enabled() -> bool {
    env_bool("SCREENSHOT_EXACT_HEIGHT_ENABLED", true)
}

fn screenshot_variant_media_features(variant: ScreenshotVariant) -> Option<Vec<MediaFeature>> {
    match variant {
        ScreenshotVariant::Dark => Some(vec![MediaFeature::new("prefers-color-scheme", "dark")]),
        ScreenshotVariant::Light => None,
    }
}

async fn apply_screenshot_variant_media_emulation(
    page: &Page,
    variant: ScreenshotVariant,
) -> Result<(), String> {
    let Some(features) = screenshot_variant_media_features(variant) else {
        return Ok(());
    };

    page.emulate_media_features(features)
        .await
        .map(|_| ())
        .map_err(|err| {
            format!("failed to emulate chromium media features for screenshot variant: {err}")
        })
}

async fn capture_single_screenshot_fixed_viewport(
    url: &str,
    viewport: Viewport,
    variant: ScreenshotVariant,
) -> Result<Vec<u8>, String> {
    let timeout = screenshot_timeout();
    let session = ChromiumoxideSession::launch(
        viewport,
        timeout,
        Some(variant),
        screenshot_render_wait_ms(),
    )
    .await?;
    let capture_result = tokio::time::timeout(timeout, async {
        let page = session
            .browser
            .new_page("about:blank")
            .await
            .map_err(|err| format!("failed to create chromium page for screenshot: {err}"))?;
        apply_screenshot_variant_media_emulation(&page, variant).await?;
        page.goto(url)
            .await
            .map_err(|err| format!("chromium screenshot navigation failed: {err}"))?;
        sleep(Duration::from_millis(screenshot_render_wait_ms())).await;
        page.screenshot(screenshot_params(false))
            .await
            .map_err(|err| format!("failed to capture chromium screenshot: {err}"))
    })
    .await
    .map_err(|_| format!("screenshot capture timed out after {}s", timeout.as_secs()))?;

    let shutdown_warnings = session.shutdown().await;
    log_chromium_shutdown_warnings(&shutdown_warnings);
    let screenshot_bytes = capture_result?;
    if screenshot_bytes.is_empty() {
        return Err("chromium created an empty screenshot payload".to_string());
    }
    Ok(screenshot_bytes)
}

fn screenshot_profile_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let jitter = jitter_ms();
    std::env::temp_dir().join(format!(
        "hyperlinked-screenshot-profile-{nanos:x}-{jitter:x}"
    ))
}

async fn maybe_sweep_stale_screenshot_profiles() {
    if SCREENSHOT_PROFILE_SWEEP_ONCE.set(()).is_err() {
        return;
    }

    let Ok(mut entries) = tokio::fs::read_dir(std::env::temp_dir()).await else {
        return;
    };
    let now = SystemTime::now();

    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "failed while scanning temporary directory for stale screenshot profiles"
                );
                break;
            }
        };

        let path = entry.path();
        if !looks_like_screenshot_profile_dir(path.as_path()) {
            continue;
        }

        let Ok(metadata) = entry.metadata().await else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }

        let Ok(modified_at) = metadata.modified() else {
            continue;
        };
        let Ok(age) = now.duration_since(modified_at) else {
            continue;
        };
        if age < SCREENSHOT_PROFILE_STALE_MAX_AGE {
            continue;
        }

        if let Err(error) = remove_directory_with_retries(
            path.as_path(),
            SCREENSHOT_PROFILE_CLEANUP_RETRY_ATTEMPTS,
            SCREENSHOT_PROFILE_CLEANUP_RETRY_BASE_DELAY_MS,
        )
        .await
        {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "failed to remove stale screenshot profile"
            );
        }
    }
}

fn looks_like_screenshot_profile_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("hyperlinked-screenshot-profile-"))
}

async fn remove_directory_with_retries(
    path: &Path,
    attempts: usize,
    retry_base_delay_ms: u64,
) -> Result<(), String> {
    let max_attempts = attempts.max(1);
    let mut last_error = None;

    for attempt in 1..=max_attempts {
        match tokio::fs::remove_dir_all(path).await {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                if attempt < max_attempts {
                    sleep(Duration::from_millis(
                        retry_base_delay_ms.saturating_mul(attempt as u64),
                    ))
                    .await;
                }
            }
        }
    }

    if let Some(error) = last_error {
        return Err(error.to_string());
    }
    Ok(())
}

fn screenshot_chromium_path() -> String {
    chromium_path()
}

fn chromium_launch_args(variant: Option<ScreenshotVariant>, render_wait_ms: u64) -> Vec<String> {
    let mut args = vec![
        "--disable-gpu".to_string(),
        "--hide-scrollbars".to_string(),
        "--run-all-compositor-stages-before-draw".to_string(),
        format!("--virtual-time-budget={render_wait_ms}"),
        "--disable-dev-shm-usage".to_string(),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--no-sandbox".to_string(),
        "--disable-setuid-sandbox".to_string(),
    ];
    if matches!(variant, Some(ScreenshotVariant::Dark)) {
        args.push("--force-dark-mode".to_string());
        args.push("--enable-features=WebContentsForceDark".to_string());
    }
    args
}

fn chromium_path() -> String {
    if let Some(configured_path) = std::env::var(CHROMIUM_PATH_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        if command_looks_available(&configured_path) {
            return configured_path;
        }
        tracing::warn!(
            chromium_path = %configured_path,
            "configured CHROMIUM_PATH was not executable; falling back to autodetected chromium candidates"
        );
    }

    for candidate in chromium_binary_candidates() {
        if command_looks_available(candidate) {
            return candidate.to_string();
        }
    }

    "chromium".to_string()
}

fn chromium_binary_candidates() -> [&'static str; 9] {
    [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/snap/bin/chromium",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ]
}

fn command_looks_available(command: &str) -> bool {
    if command.contains(std::path::MAIN_SEPARATOR) {
        return is_executable_file(Path::new(command));
    }

    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths)
            .map(|path| path.join(command))
            .any(|path| is_executable_file(path.as_path()))
    })
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

async fn ensure_chromium_runtime_dir_at(runtime_dir: &Path) -> Result<(), String> {
    tokio::fs::create_dir_all(runtime_dir)
        .await
        .map_err(|err| {
            format!(
                "failed to create chromium runtime directory {}: {err}",
                runtime_dir.display()
            )
        })?;
    #[cfg(unix)]
    tokio::fs::set_permissions(runtime_dir, std::fs::Permissions::from_mode(0o700))
        .await
        .map_err(|err| {
            format!(
                "failed to set chromium runtime permissions {}: {err}",
                runtime_dir.display()
            )
        })?;
    Ok(())
}

fn chromium_runtime_dir() -> PathBuf {
    if let Some(path) = std::env::var(CHROMIUM_RUNTIME_DIR_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return PathBuf::from(path);
    }

    std::env::temp_dir().join(DEFAULT_CHROMIUM_RUNTIME_DIR)
}

fn snapshot_content_timeout() -> Duration {
    Duration::from_secs(env_u64(
        "SNAPSHOT_CONTENT_TIMEOUT_SECS",
        DEFAULT_SNAPSHOT_CONTENT_TIMEOUT_SECS,
        1,
        120,
    ))
}

fn snapshot_content_render_wait_ms() -> u64 {
    env_u64(
        "SNAPSHOT_CONTENT_RENDER_WAIT_MS",
        DEFAULT_SNAPSHOT_CONTENT_RENDER_WAIT_MS,
        0,
        60_000,
    )
}

fn screenshot_timeout() -> Duration {
    Duration::from_secs(env_u64(
        "SCREENSHOT_TIMEOUT_SECS",
        DEFAULT_SCREENSHOT_TIMEOUT_SECS,
        1,
        120,
    ))
}

fn screenshot_desktop_viewport() -> Viewport {
    parse_viewport_env(
        "SCREENSHOT_DESKTOP_VIEWPORT",
        DEFAULT_SCREENSHOT_DESKTOP_VIEWPORT,
    )
}

fn content_capture_viewport() -> Viewport {
    parse_viewport_env(
        "SNAPSHOT_CONTENT_VIEWPORT",
        DEFAULT_SNAPSHOT_CONTENT_VIEWPORT,
    )
}

fn screenshot_thumbnail_size() -> u32 {
    env_u64(
        "SCREENSHOT_THUMB_SIZE",
        DEFAULT_SCREENSHOT_THUMB_SIZE as u64,
        64,
        2048,
    ) as u32
}

fn screenshot_render_wait_ms() -> u64 {
    env_u64(
        "SCREENSHOT_RENDER_WAIT_MS",
        DEFAULT_SCREENSHOT_RENDER_WAIT_MS,
        0,
        60_000,
    )
}

fn screenshot_exact_height_attempts() -> usize {
    env_u64(
        "SCREENSHOT_EXACT_HEIGHT_ATTEMPTS",
        DEFAULT_SCREENSHOT_EXACT_HEIGHT_ATTEMPTS,
        1,
        10,
    ) as usize
}

fn screenshot_fixed_viewport_attempts() -> usize {
    env_u64(
        "SCREENSHOT_FIXED_VIEWPORT_ATTEMPTS",
        DEFAULT_SCREENSHOT_FIXED_VIEWPORT_ATTEMPTS,
        1,
        10,
    ) as usize
}

fn screenshot_retry_base_backoff_ms() -> u64 {
    env_u64(
        "SCREENSHOT_RETRY_BASE_BACKOFF_MS",
        DEFAULT_SCREENSHOT_RETRY_BASE_BACKOFF_MS,
        0,
        10_000,
    )
}

fn screenshot_retry_jitter_max_ms() -> u64 {
    env_u64(
        "SCREENSHOT_RETRY_JITTER_MAX_MS",
        DEFAULT_SCREENSHOT_RETRY_JITTER_MAX_MS,
        0,
        10_000,
    )
}

fn screenshot_retry_backoff_delay(attempt: usize) -> Duration {
    let exponent = attempt.saturating_sub(1).min(6) as u32;
    let base = screenshot_retry_base_backoff_ms().saturating_mul(1u64 << exponent);
    let jitter = jitter_ms_with_max(screenshot_retry_jitter_max_ms());
    Duration::from_millis(base.saturating_add(jitter))
}

fn should_skip_fixed_viewport_fallback_for_exact_error(error: &str) -> bool {
    is_chromium_startup_or_environment_error(error)
}

fn log_chromium_shutdown_warnings(warnings: &[String]) {
    for warning in warnings {
        tracing::warn!(warning = %warning, "chromium session shutdown warning");
    }
}

fn exact_height_capture_error_retryable(error: &str) -> bool {
    screenshot_capture_error_retryable(error) && !is_websocket_connection_reset_error(error)
}

fn screenshot_capture_error_retryable(error: &str) -> bool {
    let normalized = error.to_ascii_lowercase();
    if normalized.contains("invalid screenshot url")
        || is_chromium_startup_or_environment_error(error)
        || normalized.contains("page-height evaluation raised an exception")
        || normalized.contains("page-height evaluation returned a non-numeric result")
        || normalized.contains("page-height evaluation returned an invalid value")
        || is_exact_height_empty_payload_error(error)
        || normalized.contains("failed to decode chromium devtools response")
        || normalized.contains("failed to decode chromium screenshot payload")
    {
        return false;
    }

    true
}

fn is_chromium_startup_or_environment_error(error: &str) -> bool {
    let normalized = error.to_ascii_lowercase();
    normalized.contains("failed to launch chromium browser")
        || normalized.contains("timed out waiting for chromium devtools endpoint")
        || normalized.contains("running as root without --no-sandbox is not supported")
        || normalized.contains("zygote_host_impl_linux.cc")
        || normalized.contains("failed to create chromium runtime directory")
        || normalized.contains("failed to set chromium runtime permissions")
        || normalized.contains("/run/user/0")
}

fn is_exact_height_empty_payload_error(error: &str) -> bool {
    error
        .trim()
        .eq_ignore_ascii_case(EXACT_HEIGHT_EMPTY_PAYLOAD_ERROR)
}

fn is_websocket_connection_reset_error(error: &str) -> bool {
    let normalized = error.to_ascii_lowercase();
    normalized.contains("websocket protocol error: connection reset without closing handshake")
}

fn screenshot_dark_mode_enabled() -> bool {
    env_bool("SCREENSHOT_DARK_MODE_ENABLED", true)
}

fn current_screenshot_font_diagnostics() -> Option<ScreenshotFontDiagnostics> {
    font_diagnostics::current().screenshot_artifact_context()
}

fn encode_screenshot_failure_payload(
    source_url: &str,
    errors: Vec<String>,
    attempts: Option<Vec<ScreenshotCaptureAttempt>>,
    payload_kind: &str,
) -> Vec<u8> {
    let font_diagnostics = current_screenshot_font_diagnostics();
    serde_json::to_vec_pretty(&ScreenshotFailureArtifact {
        source_url: source_url.to_string(),
        failed_at: now_utc().to_string(),
        errors,
        chromium_path: screenshot_chromium_path(),
        timeout_secs: screenshot_timeout().as_secs(),
        attempts,
        font_diagnostics,
    })
    .unwrap_or_else(|encode_error| {
        format!(
            "{{\"error\":\"failed to encode screenshot {payload_kind} payload: {encode_error}\"}}"
        )
        .into_bytes()
    })
}

fn parse_viewport_env(key: &str, default: Viewport) -> Viewport {
    let Some(raw) = std::env::var(key).ok() else {
        return default;
    };

    let normalized = raw.trim().replace(',', "x");
    let mut parts = normalized.split('x');
    let Some(width_raw) = parts.next() else {
        return default;
    };
    let Some(height_raw) = parts.next() else {
        return default;
    };
    if parts.next().is_some() {
        return default;
    }

    let Ok(width) = width_raw.trim().parse::<u32>() else {
        return default;
    };
    let Ok(height) = height_raw.trim().parse::<u32>() else {
        return default;
    };

    if width == 0 || height == 0 {
        return default;
    }

    Viewport { width, height }
}

fn build_square_thumbnail(source_image_bytes: &[u8], size: u32) -> Result<Vec<u8>, String> {
    let image = image::load_from_memory(source_image_bytes)
        .map_err(|err| format!("invalid screenshot image payload: {err}"))?;
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return Err("invalid screenshot dimensions".to_string());
    }

    let side = width.min(height);
    let x = (width.saturating_sub(side)) / 2;
    let y = 0;
    let square = image.crop_imm(x, y, side, side);
    let thumbnail = square.resize_exact(size, size, FilterType::Lanczos3);
    encode_webp_from_dynamic_image(&thumbnail)
}

fn build_pdf_thumbnail_from_source(
    pdf_payload: &[u8],
    thumbnail_size: u32,
) -> Result<Vec<u8>, String> {
    let pdf = Pdf::new(Arc::new(pdf_payload.to_vec()))
        .map_err(|error| format!("failed to parse pdf for thumbnail rendering: {error:?}"))?;
    let page = pdf
        .pages()
        .first()
        .ok_or_else(|| "pdf did not contain any pages".to_string())?;

    let (page_width, page_height) = page.render_dimensions();
    if !(page_width.is_finite() && page_height.is_finite())
        || page_width <= 0.0
        || page_height <= 0.0
    {
        return Err(format!(
            "pdf page dimensions were invalid: width={page_width}, height={page_height}"
        ));
    }

    // Render above thumbnail resolution to preserve text legibility before square crop.
    let target_longest_edge = thumbnail_size.saturating_mul(3).clamp(thumbnail_size, 2048);
    let longest_edge = page_width.max(page_height).max(1.0);
    let scale = (target_longest_edge as f32 / longest_edge).clamp(0.1, 8.0);
    let width = (page_width * scale).round().clamp(1.0, u16::MAX as f32) as u16;
    let height = (page_height * scale).round().clamp(1.0, u16::MAX as f32) as u16;

    let pixmap = hayro_render(
        page,
        &InterpreterSettings::default(),
        &HayroRenderSettings {
            x_scale: scale,
            y_scale: scale,
            width: Some(width),
            height: Some(height),
            bg_color: WHITE,
        },
    );
    let png = pixmap
        .into_png()
        .map_err(|error| format!("failed to encode rendered pdf thumbnail as png: {error}"))?;
    let rendered_webp = encode_webp_from_image_bytes(&png)?;
    build_square_thumbnail(&rendered_webp, thumbnail_size)
}

fn build_pdf_thumbnails_from_source(
    pdf_payload: &[u8],
    thumbnail_size: u32,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let light_thumbnail = build_pdf_thumbnail_from_source(pdf_payload, thumbnail_size)?;
    let dark_thumbnail = build_dark_thumbnail_from_light_thumbnail(&light_thumbnail)?;
    Ok((light_thumbnail, dark_thumbnail))
}

fn build_dark_thumbnail_from_light_thumbnail(source_image_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let image = image::load_from_memory(source_image_bytes)
        .map_err(|err| format!("invalid screenshot image payload: {err}"))?;
    let mut rgba = image.to_rgba8();
    for pixel in rgba.pixels_mut() {
        let [r, g, b, a] = pixel.0;
        pixel.0 = [
            255u8.saturating_sub(r),
            255u8.saturating_sub(g),
            255u8.saturating_sub(b),
            a,
        ];
    }
    encode_webp_from_dynamic_image(&image::DynamicImage::ImageRgba8(rgba))
}

fn encode_webp_from_image_bytes(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let image = image::load_from_memory(bytes)
        .map_err(|err| format!("invalid screenshot image payload: {err}"))?;
    encode_webp_from_dynamic_image(&image)
}

fn encode_webp_from_dynamic_image(image: &image::DynamicImage) -> Result<Vec<u8>, String> {
    let rgba = image.to_rgba8();
    let encoded = webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height())
        .encode(SCREENSHOT_WEBP_QUALITY);
    if encoded.is_empty() {
        return Err("webp encoding produced an empty payload".to_string());
    }
    Ok(encoded.to_vec())
}

async fn fetch_response_with_retry(
    client: &reqwest::Client,
    url: Url,
    max_bytes: usize,
    deadline: Instant,
) -> Result<FetchedResponse, RetryFailure> {
    ensure_fetchable_url(&url)
        .await
        .map_err(|error| RetryFailure {
            attempts: vec![SnapshotFetchAttempt {
                attempt: 1,
                url: url.to_string(),
                elapsed_ms: 0,
                error: error.clone(),
                retryable: false,
                status: None,
            }],
            final_error: FetchAttemptError {
                message: error,
                retryable: false,
                status: None,
            },
        })?;

    let mut attempts = Vec::new();
    let url_string = url.to_string();

    for attempt in 1..=RETRY_ATTEMPTS {
        if Instant::now() >= deadline {
            let final_error = FetchAttemptError {
                message: format!("request deadline reached for {url_string}"),
                retryable: false,
                status: None,
            };
            attempts.push(SnapshotFetchAttempt {
                attempt,
                url: url_string.clone(),
                elapsed_ms: 0,
                error: final_error.message.clone(),
                retryable: false,
                status: None,
            });
            return Err(RetryFailure {
                attempts,
                final_error,
            });
        }

        let started = Instant::now();
        match fetch_response_once(client, url.clone(), max_bytes).await {
            Ok(response) => return Ok(response),
            Err(error) => {
                let elapsed_ms = started.elapsed().as_millis();
                let attempt_record = SnapshotFetchAttempt {
                    attempt,
                    url: url_string.clone(),
                    elapsed_ms,
                    error: error.message.clone(),
                    retryable: error.retryable,
                    status: error.status,
                };
                attempts.push(attempt_record);

                let should_retry =
                    error.retryable && attempt < RETRY_ATTEMPTS && Instant::now() < deadline;
                if !should_retry {
                    return Err(RetryFailure {
                        attempts,
                        final_error: error,
                    });
                }

                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(RetryFailure {
                        attempts,
                        final_error: error,
                    });
                }

                let delay = retry_backoff_delay(attempt).min(remaining);
                if !delay.is_zero() {
                    sleep(delay).await;
                }
            }
        }
    }

    Err(RetryFailure {
        attempts,
        final_error: FetchAttemptError {
            message: format!("request failed for {url_string} after {RETRY_ATTEMPTS} attempts"),
            retryable: false,
            status: None,
        },
    })
}

async fn fetch_response_once(
    client: &reqwest::Client,
    url: Url,
    max_bytes: usize,
) -> Result<FetchedResponse, FetchAttemptError> {
    let mut response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|err| FetchAttemptError {
            message: format!("request failed for {url}: {err}"),
            retryable: is_transient_reqwest_error(&err),
            status: None,
        })?;

    if !response.status().is_success() {
        let status = response.status().as_u16();
        let retryable = status == 429 || (500..=599).contains(&status);
        return Err(FetchAttemptError {
            message: format!("request failed for {url}: status {status}"),
            retryable,
            status: Some(status),
        });
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);

    let mut truncated = false;
    let mut body = Vec::with_capacity(4096);
    while let Some(chunk) = response.chunk().await.map_err(|err| FetchAttemptError {
        message: format!("failed reading body for {url}: {err}"),
        retryable: is_transient_reqwest_error(&err),
        status: None,
    })? {
        if body.len() >= max_bytes {
            truncated = true;
            break;
        }

        let remaining = max_bytes - body.len();
        if chunk.len() > remaining {
            body.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }

        body.extend_from_slice(&chunk);
    }

    Ok(FetchedResponse {
        url,
        status: response.status().as_u16(),
        content_type,
        body,
        truncated,
    })
}

fn is_transient_reqwest_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || error.is_request() || error.is_body()
}

fn retry_backoff_delay(attempt: usize) -> Duration {
    let exponent = attempt.saturating_sub(1).min(6) as u32;
    let base = RETRY_BASE_BACKOFF_MS.saturating_mul(1u64 << exponent);
    let jitter = jitter_ms_with_max(RETRY_JITTER_MAX_MS);
    Duration::from_millis(base.saturating_add(jitter))
}

fn jitter_ms() -> u64 {
    jitter_ms_with_max(RETRY_JITTER_MAX_MS)
}

fn jitter_ms_with_max(max_jitter_ms: u64) -> u64 {
    if max_jitter_ms == 0 {
        return 0;
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos() as u64)
        .unwrap_or(0);
    nanos % (max_jitter_ms + 1)
}

fn format_retry_failure(error: &RetryFailure) -> String {
    format!(
        "{} (attempts={})",
        error.final_error.message,
        error.attempts.len()
    )
}

fn snapshot_capture_error(
    source_url: &str,
    stage: &str,
    message: String,
    attempts: Vec<SnapshotFetchAttempt>,
) -> SnapshotCaptureError {
    SnapshotCaptureError {
        message: message.clone(),
        artifact: SnapshotFailureArtifact {
            source_url: source_url.to_string(),
            failed_at: now_utc().to_string(),
            stage: stage.to_string(),
            final_error: message,
            attempts,
            retry_policy: retry_policy(),
        },
    }
}

fn retry_policy() -> SnapshotRetryPolicy {
    SnapshotRetryPolicy {
        max_attempts: RETRY_ATTEMPTS,
        request_timeout_secs: REQUEST_TIMEOUT.as_secs(),
        snapshot_deadline_secs: SNAPSHOT_DEADLINE.as_secs(),
        backoff_base_ms: RETRY_BASE_BACKOFF_MS,
        jitter_max_ms: RETRY_JITTER_MAX_MS,
    }
}

fn classify_source_kind(content_type: Option<&str>, payload: &[u8]) -> SnapshotSourceKind {
    if payload.len() >= PDF_SIGNATURE.len() && &payload[..PDF_SIGNATURE.len()] == PDF_SIGNATURE {
        return SnapshotSourceKind::Pdf;
    }

    if content_type.is_some_and(is_pdf_content_type) {
        return SnapshotSourceKind::Pdf;
    }

    match content_type {
        Some(content_type) if is_html_content_type(content_type) => SnapshotSourceKind::Html,
        Some(_) => SnapshotSourceKind::Unsupported,
        None => SnapshotSourceKind::Html,
    }
}

fn is_html_content_type(content_type: &str) -> bool {
    let lower = content_type.to_ascii_lowercase();
    lower.contains("text/html") || lower.contains("application/xhtml+xml")
}

fn is_pdf_content_type(content_type: &str) -> bool {
    content_type
        .to_ascii_lowercase()
        .contains("application/pdf")
}

fn is_likely_pdf_url(url: &Url) -> bool {
    let path = url.path();
    if path.to_ascii_lowercase().ends_with(".pdf") {
        return true;
    }

    url.path_segments().is_some_and(|segments| {
        segments
            .filter(|segment| !segment.is_empty())
            .any(|segment| segment.eq_ignore_ascii_case("pdf"))
    })
}

fn looks_like_pdf_viewer_dom(payload: &[u8]) -> bool {
    let lowercase = String::from_utf8_lossy(payload).to_ascii_lowercase();
    lowercase.contains("application/pdf")
        && (lowercase.contains("<embed")
            || lowercase.contains("<object")
            || lowercase.contains("pdf-viewer")
            || lowercase.contains("mhjfbmdgcfjbbpaeojofohoefgiehjai"))
}

pub(crate) async fn ensure_fetchable_url(url: &Url) -> Result<(), String> {
    match url.scheme() {
        "http" | "https" => {}
        _ => return Err("only http/https URLs are supported".to_string()),
    }

    let host = url
        .host_str()
        .ok_or_else(|| "url host is missing".to_string())?;
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return Err("localhost URLs are not allowed".to_string());
    }

    let port = url.port_or_known_default().unwrap_or(80);
    let resolved = lookup_host((host, port))
        .await
        .map_err(|err| format!("failed to resolve host: {err}"))?;

    for addr in resolved {
        if is_private_ip(addr.ip()) {
            return Err("private or loopback addresses are not allowed".to_string());
        }
    }

    Ok(())
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ipv4) => {
            ipv4.is_private()
                || ipv4.is_loopback()
                || ipv4.is_link_local()
                || ipv4.is_unspecified()
                || ipv4.is_broadcast()
                || ipv4.is_documentation()
                || ipv4.is_multicast()
                || ipv4.octets()[0] == 0
        }
        IpAddr::V6(ipv6) => {
            ipv6.is_loopback()
                || ipv6.is_unspecified()
                || ipv6.is_unique_local()
                || ipv6.is_unicast_link_local()
                || ipv6.is_multicast()
        }
    }
}

fn append_record(
    out: &mut Vec<u8>,
    warc_type: &str,
    target_uri: &str,
    content_type: &str,
    payload: &[u8],
    extra_headers: &[(&str, String)],
) {
    out.extend_from_slice(b"WARC/1.0\r\n");
    out.extend_from_slice(format!("WARC-Type: {warc_type}\r\n").as_bytes());
    out.extend_from_slice(format!("WARC-Target-URI: {target_uri}\r\n").as_bytes());
    for (name, value) in extra_headers {
        out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    out.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
    out.extend_from_slice(format!("Content-Length: {}\r\n", payload.len()).as_bytes());
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(payload);
    out.extend_from_slice(b"\r\n\r\n");
}

fn extract_stylesheet_hrefs(document: &str) -> Vec<String> {
    let lowercase = document.to_lowercase();
    let mut hrefs = Vec::new();
    let mut cursor = 0usize;

    while let Some(link_pos) = lowercase[cursor..].find("<link") {
        let start = cursor + link_pos;
        let Some(tag_end_rel) = lowercase[start..].find('>') else {
            break;
        };
        let end = start + tag_end_rel + 1;
        let tag = &document[start..end];

        if let Some(rel) = extract_attr_value(tag, "rel")
            && rel
                .split_ascii_whitespace()
                .any(|token| token.eq_ignore_ascii_case("stylesheet"))
            && let Some(href) = extract_attr_value(tag, "href")
            && !href.trim().is_empty()
        {
            hrefs.push(href);
        }

        cursor = end;
    }

    hrefs
}

fn extract_attr_value(tag: &str, attribute: &str) -> Option<String> {
    let bytes = tag.as_bytes();
    let mut cursor = 0usize;

    while cursor < bytes.len() {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() || bytes[cursor] == b'>' {
            break;
        }
        if bytes[cursor] == b'<' || bytes[cursor] == b'/' {
            cursor += 1;
            continue;
        }

        let name_start = cursor;
        while cursor < bytes.len() && is_attr_name_char(bytes[cursor]) {
            cursor += 1;
        }
        if cursor == name_start {
            cursor += 1;
            continue;
        }

        let name = &tag[name_start..cursor];
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }

        if cursor >= bytes.len() || bytes[cursor] != b'=' {
            continue;
        }
        cursor += 1;

        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            break;
        }

        let (value_start, value_end) = if bytes[cursor] == b'"' || bytes[cursor] == b'\'' {
            let quote = bytes[cursor];
            cursor += 1;
            let value_start = cursor;
            while cursor < bytes.len() && bytes[cursor] != quote {
                cursor += 1;
            }
            let value_end = cursor;
            if cursor < bytes.len() {
                cursor += 1;
            }
            (value_start, value_end)
        } else {
            let value_start = cursor;
            while cursor < bytes.len()
                && !bytes[cursor].is_ascii_whitespace()
                && bytes[cursor] != b'>'
            {
                cursor += 1;
            }
            (value_start, cursor)
        };

        if name.eq_ignore_ascii_case(attribute) {
            return Some(tag[value_start..value_end].to_string());
        }
    }

    None
}

fn is_attr_name_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':')
}

fn env_u64(key: &str, default: u64, min: u64, max: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value.clamp(min, max))
        .unwrap_or(default.clamp(min, max))
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .and_then(|value| match value.as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        })
        .unwrap_or(default)
}

fn now_utc() -> sea_orm::entity::prelude::DateTime {
    sea_orm::entity::prelude::DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn extracts_stylesheet_links() {
        let html = r#"
        <html><head>
        <link rel="stylesheet" href="/app.css">
        <link href='https://cdn.example.com/site.css' rel='preload stylesheet'>
        <link rel="icon" href="/favicon.ico">
        </head></html>
        "#;

        let hrefs = extract_stylesheet_hrefs(html);
        assert_eq!(hrefs, vec!["/app.css", "https://cdn.example.com/site.css"]);
    }

    #[test]
    fn retry_backoff_grows() {
        let first = retry_backoff_delay(1);
        let second = retry_backoff_delay(2);
        assert!(second >= first);
    }

    #[test]
    fn classifies_pdf_sources() {
        assert_eq!(
            classify_source_kind(Some("application/pdf; charset=binary"), b"not-a-pdf"),
            SnapshotSourceKind::Pdf
        );
        assert_eq!(
            classify_source_kind(Some("application/pdf"), b"%PDF-1.7"),
            SnapshotSourceKind::Pdf
        );
        assert_eq!(
            classify_source_kind(None, b"%PDF-1.6\n%"),
            SnapshotSourceKind::Pdf
        );
    }

    #[test]
    fn classifies_html_and_unsupported_sources() {
        assert_eq!(
            classify_source_kind(Some("text/html; charset=utf-8"), b"<html></html>"),
            SnapshotSourceKind::Html
        );
        assert_eq!(
            classify_source_kind(Some("text/html"), b"<html></html>"),
            SnapshotSourceKind::Html
        );
        assert_eq!(
            classify_source_kind(Some("application/json"), b"{\"ok\":true}"),
            SnapshotSourceKind::Unsupported
        );
    }

    #[test]
    fn detects_likely_pdf_urls() {
        let arxiv = Url::parse("https://arxiv.org/pdf/2602.11988").expect("valid arxiv url");
        assert!(is_likely_pdf_url(&arxiv));

        let suffix =
            Url::parse("https://example.com/files/paper.PDF").expect("valid pdf suffix url");
        assert!(is_likely_pdf_url(&suffix));
    }

    #[test]
    fn does_not_detect_non_pdf_urls_as_likely_pdf() {
        let html = Url::parse("https://example.com/posts/123").expect("valid html url");
        assert!(!is_likely_pdf_url(&html));

        let query_hint =
            Url::parse("https://example.com/download?format=pdf").expect("valid query url");
        assert!(!is_likely_pdf_url(&query_hint));
    }

    #[test]
    fn detects_pdf_viewer_dom_payloads() {
        let html = r#"
        <html><body>
          <embed src="blob:abc" type="application/pdf">
        </body></html>
        "#;
        assert!(looks_like_pdf_viewer_dom(html.as_bytes()));
    }

    #[test]
    fn does_not_flag_regular_html_as_pdf_viewer() {
        let html = r#"
        <html><body><article>hello world</article></body></html>
        "#;
        assert!(!looks_like_pdf_viewer_dom(html.as_bytes()));
    }

    #[test]
    fn skips_screenshot_capture_for_pdf_sources() {
        assert!(should_skip_screenshot_capture_for_source(
            &HyperlinkArtifactKind::PdfSource
        ));
        assert!(!should_skip_screenshot_capture_for_source(
            &HyperlinkArtifactKind::SnapshotWarc
        ));
    }

    #[test]
    fn defaults_hyperlink_source_type_to_unknown_when_unset() {
        let active = hyperlink::ActiveModel::default();
        assert_eq!(hyperlink_source_type(&active), HyperlinkSourceType::Unknown);
    }

    #[test]
    fn absolute_http_url_detection_rejects_relative_paths() {
        assert!(is_absolute_http_or_https_url(
            "https://example.com/file.pdf"
        ));
        assert!(is_absolute_http_or_https_url("http://example.com/file.pdf"));
        assert!(!is_absolute_http_or_https_url("/uploads/1/file.pdf"));
        assert!(!is_absolute_http_or_https_url("file.pdf"));
    }

    #[test]
    fn pdf_thumbnail_render_rejects_invalid_payloads() {
        let result = build_pdf_thumbnail_from_source(b"not-a-pdf", 400);
        assert!(result.is_err());
    }

    #[test]
    fn dark_thumbnail_transform_inverts_light_thumbnail_payload() {
        let light_image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2,
            2,
            image::Rgba([255, 255, 255, 255]),
        ));
        let light_webp =
            encode_webp_from_dynamic_image(&light_image).expect("light image should encode");
        let dark_webp = build_dark_thumbnail_from_light_thumbnail(&light_webp)
            .expect("dark thumbnail transform should succeed");

        let decoded_dark = image::load_from_memory(&dark_webp)
            .expect("dark thumbnail should decode")
            .to_rgb8();
        let pixel = decoded_dark.get_pixel(0, 0).0;
        assert!(pixel[0] < 80);
        assert!(pixel[1] < 80);
        assert!(pixel[2] < 80);
    }

    #[test]
    fn dark_screenshot_variant_sets_prefers_color_scheme_media_feature() {
        let features = screenshot_variant_media_features(ScreenshotVariant::Dark)
            .expect("dark variant should define media features");
        assert_eq!(features.len(), 1);
        assert_eq!(features[0].name, "prefers-color-scheme");
        assert_eq!(features[0].value, "dark");
    }

    #[test]
    fn light_screenshot_variant_does_not_set_media_features() {
        assert!(screenshot_variant_media_features(ScreenshotVariant::Light).is_none());
    }

    #[test]
    fn chromium_launch_args_include_dark_flags_for_dark_variant() {
        let args = chromium_launch_args(Some(ScreenshotVariant::Dark), 5_000);
        assert!(args.iter().any(|arg| arg == "--force-dark-mode"));
        assert!(
            args.iter()
                .any(|arg| arg == "--enable-features=WebContentsForceDark")
        );
    }

    #[test]
    fn chromium_launch_args_exclude_dark_flags_for_light_variant() {
        let args = chromium_launch_args(Some(ScreenshotVariant::Light), 5_000);
        assert!(!args.iter().any(|arg| arg == "--force-dark-mode"));
        assert!(
            !args
                .iter()
                .any(|arg| arg == "--enable-features=WebContentsForceDark")
        );
    }

    #[test]
    fn normalize_page_height_bounds_swaps_inverted_values() {
        let bounds = normalize_page_height_bounds(PageHeightBounds {
            min: 1800,
            max: 900,
        });
        assert_eq!(
            bounds,
            PageHeightBounds {
                min: 900,
                max: 1800
            }
        );
    }

    #[test]
    fn clamp_page_height_respects_bounds() {
        let bounds = PageHeightBounds {
            min: 800,
            max: 1600,
        };
        assert_eq!(clamp_page_height(200, bounds), 800);
        assert_eq!(clamp_page_height(1200, bounds), 1200);
        assert_eq!(clamp_page_height(3200, bounds), 1600);
    }

    #[test]
    fn screenshot_retry_backoff_grows() {
        let first = screenshot_retry_backoff_delay(1);
        let second = screenshot_retry_backoff_delay(2);
        assert!(second >= first);
    }

    #[test]
    fn screenshot_retryable_error_classifier_defaults_to_retry() {
        assert!(screenshot_capture_error_retryable(
            "failed to read chromium devtools response for `Runtime.evaluate`: connection reset"
        ));
        assert!(screenshot_capture_error_retryable(
            "failed to read chromium devtools response for `Runtime.evaluate`: WebSocket protocol error: Connection reset without closing handshake"
        ));
    }

    #[test]
    fn screenshot_retryable_error_classifier_rejects_deterministic_errors() {
        assert!(!screenshot_capture_error_retryable(
            "chromium page-height evaluation returned a non-numeric result"
        ));
        assert!(!screenshot_capture_error_retryable(
            EXACT_HEIGHT_EMPTY_PAYLOAD_ERROR
        ));
        assert!(!screenshot_capture_error_retryable(
            "failed to decode chromium screenshot payload: Invalid byte"
        ));
        assert!(!screenshot_capture_error_retryable(
            "failed to launch chromium browser: No such file or directory (os error 2)"
        ));
        assert!(!screenshot_capture_error_retryable(
            "timed out waiting for chromium devtools endpoint"
        ));
        assert!(!screenshot_capture_error_retryable(
            "mkdir: cannot create directory '/run/user/0': Permission denied"
        ));
    }

    #[test]
    fn exact_height_retryable_error_classifier_rejects_connection_reset() {
        assert!(!exact_height_capture_error_retryable(
            "failed to read chromium devtools response for `Emulation.setDeviceMetricsOverride`: WebSocket protocol error: Connection reset without closing handshake"
        ));
    }

    #[test]
    fn exact_height_skips_fixed_viewport_fallback_for_startup_errors() {
        assert!(should_skip_fixed_viewport_fallback_for_exact_error(
            "failed to launch chromium browser: No such file or directory (os error 2)"
        ));
        assert!(should_skip_fixed_viewport_fallback_for_exact_error(
            "timed out waiting for chromium devtools endpoint"
        ));
        assert!(!should_skip_fixed_viewport_fallback_for_exact_error(
            "chromium page-height evaluation returned a non-numeric result"
        ));
    }

    #[test]
    fn exact_height_state_disables_after_empty_payload_error() {
        let mut state = ExactHeightCaptureState::from_enabled(true);
        assert!(state.should_try_exact_height());

        let disabled = state.disable_for_job_on_error(EXACT_HEIGHT_EMPTY_PAYLOAD_ERROR);
        assert!(disabled);
        assert!(!state.should_try_exact_height());
    }

    #[test]
    fn exact_height_state_keeps_exact_height_for_other_errors() {
        let mut state = ExactHeightCaptureState::from_enabled(true);
        assert!(state.should_try_exact_height());

        let disabled =
            state.disable_for_job_on_error("exact-height screenshot capture timed out after 20s");
        assert!(!disabled);
        assert!(state.should_try_exact_height());
    }

    #[test]
    fn screenshot_failure_artifact_serializes_font_diagnostics() {
        let artifact = ScreenshotFailureArtifact {
            source_url: "https://example.com".to_string(),
            failed_at: "2026-02-26T00:00:00Z".to_string(),
            errors: vec!["screenshot failed".to_string()],
            chromium_path: "chromium".to_string(),
            timeout_secs: 20,
            attempts: None,
            font_diagnostics: Some(ScreenshotFontDiagnostics {
                fontconfig_found: false,
                required_families: vec!["Noto Sans".to_string()],
                missing_families: vec!["Noto Sans".to_string()],
                resolved_matches: vec![],
            }),
        };

        let value = serde_json::to_value(&artifact).expect("artifact should serialize");
        assert!(value.get("font_diagnostics").is_some());
    }

    #[test]
    fn screenshot_failure_artifact_serializes_attempts_when_present() {
        let artifact = ScreenshotFailureArtifact {
            source_url: "https://example.com".to_string(),
            failed_at: "2026-02-26T00:00:00Z".to_string(),
            errors: vec!["screenshot failed".to_string()],
            chromium_path: "chromium".to_string(),
            timeout_secs: 20,
            attempts: Some(vec![ScreenshotCaptureAttempt {
                attempt: 1,
                stage: ScreenshotCaptureStage::ExactHeight,
                error: "connection reset".to_string(),
                retryable: true,
            }]),
            font_diagnostics: None,
        };

        let value = serde_json::to_value(&artifact).expect("artifact should serialize");
        assert!(value.get("attempts").is_some());
    }

    struct FakeBrowserLifecycle {
        close_result: Result<(), String>,
        wait_results: VecDeque<Result<Option<()>, String>>,
        kill_result: Result<bool, String>,
        close_calls: usize,
        wait_calls: usize,
        kill_calls: usize,
    }

    impl FakeBrowserLifecycle {
        fn with_results(
            close_result: Result<(), String>,
            wait_results: Vec<Result<Option<()>, String>>,
            kill_result: Result<bool, String>,
        ) -> Self {
            Self {
                close_result,
                wait_results: wait_results.into_iter().collect(),
                kill_result,
                close_calls: 0,
                wait_calls: 0,
                kill_calls: 0,
            }
        }
    }

    #[async_trait::async_trait]
    impl BrowserLifecycle for FakeBrowserLifecycle {
        async fn close_browser(&mut self) -> Result<(), String> {
            self.close_calls += 1;
            self.close_result.clone()
        }

        async fn wait_for_exit(&mut self) -> Result<Option<()>, String> {
            self.wait_calls += 1;
            self.wait_results.pop_front().unwrap_or(Ok(Some(())))
        }

        async fn kill_browser(&mut self) -> Result<bool, String> {
            self.kill_calls += 1;
            self.kill_result.clone()
        }
    }

    #[tokio::test]
    async fn shutdown_browser_lifecycle_reaps_after_close_without_kill() {
        let mut lifecycle =
            FakeBrowserLifecycle::with_results(Ok(()), vec![Ok(Some(()))], Ok(true));
        let warnings = shutdown_browser_lifecycle(&mut lifecycle, Path::new("/tmp/profile")).await;

        assert!(warnings.is_empty());
        assert_eq!(lifecycle.close_calls, 1);
        assert_eq!(lifecycle.wait_calls, 1);
        assert_eq!(lifecycle.kill_calls, 0);
    }

    #[tokio::test]
    async fn shutdown_browser_lifecycle_kills_when_close_wait_fails() {
        let mut lifecycle = FakeBrowserLifecycle::with_results(
            Err("close failed".to_string()),
            vec![Err("wait after close failed".to_string()), Ok(Some(()))],
            Ok(true),
        );
        let warnings = shutdown_browser_lifecycle(&mut lifecycle, Path::new("/tmp/profile")).await;

        assert_eq!(lifecycle.close_calls, 1);
        assert_eq!(lifecycle.wait_calls, 2);
        assert_eq!(lifecycle.kill_calls, 1);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("failed to close chromium browser"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("wait after close failed"))
        );
        assert!(
            !warnings
                .iter()
                .any(|warning| warning.contains("not confirmed as reaped"))
        );
    }

    #[tokio::test]
    async fn shutdown_browser_lifecycle_warns_when_reap_not_confirmed() {
        let mut lifecycle = FakeBrowserLifecycle::with_results(
            Ok(()),
            vec![
                Err("wait close failed".to_string()),
                Err("wait kill failed".to_string()),
            ],
            Ok(false),
        );
        let warnings = shutdown_browser_lifecycle(&mut lifecycle, Path::new("/tmp/profile")).await;

        assert_eq!(lifecycle.close_calls, 1);
        assert_eq!(lifecycle.wait_calls, 2);
        assert_eq!(lifecycle.kill_calls, 1);
        assert!(warnings.iter().any(|warning| {
            warning.contains("kill was skipped because no child process handle was available")
        }));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("not confirmed as reaped"))
        );
    }

    #[tokio::test]
    async fn remove_directory_with_retries_removes_nested_directories() {
        use std::{fs, time::SystemTime};

        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("hyperlinked-remove-dir-test-{unique}"));
        let nested = root.join("nested");

        fs::create_dir_all(&nested).expect("nested test directory should be created");
        fs::write(nested.join("child.txt"), b"ok").expect("test file should be written");

        remove_directory_with_retries(root.as_path(), 2, 1)
            .await
            .expect("directory removal should succeed");
        assert!(!root.exists());
    }
}
