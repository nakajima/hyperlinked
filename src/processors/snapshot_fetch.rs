use image::{GenericImageView, imageops::FilterType};
use reqwest::{Url, header::CONTENT_TYPE};
use sea_orm::DatabaseConnection;
use serde::Serialize;
use std::collections::HashSet;
use std::io::Cursor;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::{net::lookup_host, process::Command, time::sleep};

use crate::{
    entity::{hyperlink, hyperlink_artifact::HyperlinkArtifactKind},
    model::hyperlink_artifact,
    processors::processor::{ProcessingError, Processor},
};

const MAX_HTML_BYTES: usize = 5 * 1024 * 1024;
const MAX_CSS_FILES: usize = 25;
const MAX_CSS_BYTES_PER_FILE: usize = 1024 * 1024;
const MAX_TOTAL_CSS_BYTES: usize = 20 * 1024 * 1024;

const SNAPSHOT_CONTENT_TYPE: &str = "application/warc";
const SNAPSHOT_ERROR_CONTENT_TYPE: &str = "application/json";
const PDF_SOURCE_DEFAULT_CONTENT_TYPE: &str = "application/pdf";
const SCREENSHOT_CONTENT_TYPE: &str = "image/png";
const SCREENSHOT_ERROR_CONTENT_TYPE: &str = "application/json";

const RETRY_ATTEMPTS: usize = 4;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const SNAPSHOT_DEADLINE: Duration = Duration::from_secs(30);
const RETRY_BASE_BACKOFF_MS: u64 = 200;
const RETRY_JITTER_MAX_MS: u64 = 125;
const DEFAULT_SNAPSHOT_CONTENT_TIMEOUT_SECS: u64 = 20;
const DEFAULT_SNAPSHOT_CONTENT_RENDER_WAIT_MS: u64 = 5000;
const DEFAULT_SCREENSHOT_TIMEOUT_SECS: u64 = 20;
const CHROMIUM_PATH_ENV: &str = "CHROMIUM_PATH";
const DEFAULT_SCREENSHOT_DESKTOP_VIEWPORT: Viewport = Viewport {
    width: 1366,
    height: 4096,
};
const DEFAULT_SCREENSHOT_THUMB_SIZE: u32 = 400;
const DEFAULT_SCREENSHOT_RENDER_WAIT_MS: u64 = 5000;

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

        match capture_snapshot(hyperlink.url.as_ref()).await {
            Ok(capture) => {
                let (kind, payload, content_type) = match capture {
                    SnapshotCapture::Html { archive } => (
                        HyperlinkArtifactKind::SnapshotWarc,
                        archive,
                        SNAPSHOT_CONTENT_TYPE.to_string(),
                    ),
                    SnapshotCapture::Pdf {
                        payload,
                        content_type,
                    } => (HyperlinkArtifactKind::PdfSource, payload, content_type),
                };

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

                match capture_screenshots(&source_url).await {
                    Ok(capture) => {
                        let screenshot_artifact = hyperlink_artifact::insert(
                            connection,
                            hyperlink_id,
                            Some(self.job_id),
                            HyperlinkArtifactKind::ScreenshotPng,
                            capture.desktop_png,
                            SCREENSHOT_CONTENT_TYPE,
                        )
                        .await
                        .map_err(ProcessingError::DB)?;
                        output.screenshot_artifact_id = Some(screenshot_artifact.id);

                        if let Some(dark_png) = capture.desktop_dark_png {
                            let dark_artifact = hyperlink_artifact::insert(
                                connection,
                                hyperlink_id,
                                Some(self.job_id),
                                HyperlinkArtifactKind::ScreenshotDarkPng,
                                dark_png,
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
                            HyperlinkArtifactKind::ScreenshotThumbPng,
                            capture.thumbnail_png,
                            SCREENSHOT_CONTENT_TYPE,
                        )
                        .await
                        .map_err(ProcessingError::DB)?;
                        output.screenshot_thumb_artifact_id = Some(thumbnail_artifact.id);

                        if let Some(dark_thumbnail_png) = capture.thumbnail_dark_png {
                            let dark_thumbnail_artifact = hyperlink_artifact::insert(
                                connection,
                                hyperlink_id,
                                Some(self.job_id),
                                HyperlinkArtifactKind::ScreenshotThumbDarkPng,
                                dark_thumbnail_png,
                                SCREENSHOT_CONTENT_TYPE,
                            )
                            .await
                            .map_err(ProcessingError::DB)?;
                            output.screenshot_thumb_dark_artifact_id =
                                Some(dark_thumbnail_artifact.id);
                        }

                        if !capture.warnings.is_empty() {
                            let payload = serde_json::to_vec_pretty(&ScreenshotFailureArtifact {
                                source_url: source_url.clone(),
                                failed_at: now_utc().to_string(),
                                errors: capture.warnings,
                                chromium_path: screenshot_chromium_path(),
                                timeout_secs: screenshot_timeout().as_secs(),
                            })
                            .unwrap_or_else(|encode_error| {
                                format!(
                                    "{{\"error\":\"failed to encode screenshot warning payload: {encode_error}\"}}"
                                )
                                .into_bytes()
                            });
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
                        let payload = serde_json::to_vec_pretty(&ScreenshotFailureArtifact {
                            source_url: source_url.clone(),
                            failed_at: now_utc().to_string(),
                            errors: vec![error],
                            chromium_path: screenshot_chromium_path(),
                            timeout_secs: screenshot_timeout().as_secs(),
                        })
                        .unwrap_or_else(|encode_error| {
                            format!(
                                "{{\"error\":\"failed to encode screenshot error payload: {encode_error}\"}}"
                            )
                            .into_bytes()
                        });

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
    desktop_png: Vec<u8>,
    desktop_dark_png: Option<Vec<u8>>,
    thumbnail_png: Vec<u8>,
    thumbnail_dark_png: Option<Vec<u8>>,
    warnings: Vec<String>,
}

#[derive(Clone, Copy)]
struct Viewport {
    width: u32,
    height: u32,
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

    if snapshot_content_use_chromium() && !path_has_pdf_extension(parsed.path()) {
        match capture_html_with_chromium_response(parsed.clone(), deadline).await {
            Ok(source_response) => {
                if looks_like_pdf_viewer_dom(&source_response.body)
                    && let Ok(capture) = capture_snapshot_via_reqwest(
                        &client,
                        parsed.clone(),
                        deadline,
                        HtmlCaptureMethod::ReqwestFallback,
                        true,
                        Some(
                            "chromium dump-dom looked like a PDF viewer; attempted reqwest source capture"
                                .to_string(),
                        ),
                    )
                    .await
                {
                    return Ok(capture);
                }

                return capture_snapshot_from_html_response(
                    &client,
                    source_response,
                    deadline,
                    HtmlCaptureMethod::Chromium,
                    false,
                    None,
                )
                .await;
            }
            Err(chromium_error) => {
                match capture_snapshot_via_reqwest(
                    &client,
                    parsed.clone(),
                    deadline,
                    HtmlCaptureMethod::ReqwestFallback,
                    true,
                    Some(chromium_error.clone()),
                )
                .await
                {
                    Ok(capture) => return Ok(capture),
                    Err(mut fallback_error) => {
                        let message = format!(
                            "chromium content capture failed: {chromium_error}; reqwest fallback failed: {}",
                            fallback_error.message
                        );
                        fallback_error.message = message.clone();
                        fallback_error.artifact.stage = "chromium_fallback".to_string();
                        fallback_error.artifact.final_error = message;
                        return Err(fallback_error);
                    }
                }
            }
        }
    }

    capture_snapshot_via_reqwest(
        &client,
        parsed,
        deadline,
        HtmlCaptureMethod::Reqwest,
        false,
        None,
    )
    .await
}

async fn capture_snapshot_via_reqwest(
    client: &reqwest::Client,
    parsed: Url,
    deadline: Instant,
    capture_method: HtmlCaptureMethod,
    fallback_used: bool,
    chromium_error: Option<String>,
) -> Result<SnapshotCapture, SnapshotCaptureError> {
    let source_response =
        fetch_response_with_retry(client, parsed.clone(), MAX_HTML_BYTES, deadline)
            .await
            .map_err(|failure| {
                snapshot_capture_error(
                    parsed.as_str(),
                    "html_fetch",
                    failure.final_error.message,
                    failure.attempts,
                )
            })?;

    let source_kind = classify_source_kind(
        source_response.content_type.as_deref(),
        source_response.url.path(),
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

    let mut command = Command::new(snapshot_content_chromium_path());
    command
        .arg("--headless=new")
        .arg("--disable-gpu")
        .arg("--dump-dom")
        .arg("--run-all-compositor-stages-before-draw")
        .arg(format!(
            "--virtual-time-budget={}",
            snapshot_content_render_wait_ms()
        ))
        .arg(url.as_str());

    let timeout =
        snapshot_content_timeout().min(deadline.saturating_duration_since(Instant::now()));
    if timeout.is_zero() {
        return Err("snapshot deadline reached before chromium content capture".to_string());
    }

    let output = tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| {
            format!(
                "chromium content capture timed out after {}s",
                timeout.as_secs()
            )
        })?
        .map_err(|err| format!("failed to launch chromium for content capture: {err}"))?;

    if !output.status.success() {
        let stderr = truncate_for_error_message(&String::from_utf8_lossy(&output.stderr), 400);
        let stdout = truncate_for_error_message(&String::from_utf8_lossy(&output.stdout), 200);
        return Err(format!(
            "chromium content capture failed with status {}: stderr={stderr}, stdout={stdout}",
            output.status
        ));
    }

    let mut body = output.stdout;
    if body.is_empty() {
        return Err("chromium content capture produced an empty DOM".to_string());
    }

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

async fn capture_screenshots(url: &str) -> Result<ScreenshotCapture, String> {
    let parsed = Url::parse(url).map_err(|err| format!("invalid screenshot url: {err}"))?;
    ensure_fetchable_url(&parsed).await?;

    let desktop_viewport = screenshot_desktop_viewport();
    let desktop_png =
        capture_single_screenshot(parsed.as_str(), desktop_viewport, ScreenshotVariant::Light)
            .await?;
    let thumbnail_png = build_square_thumbnail(&desktop_png, screenshot_thumbnail_size())?;

    let mut warnings = Vec::new();
    let (desktop_dark_png, thumbnail_dark_png) = if screenshot_dark_mode_enabled() {
        match capture_single_screenshot(parsed.as_str(), desktop_viewport, ScreenshotVariant::Dark)
            .await
        {
            Ok(bytes) => {
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
                warnings.push(format!("dark screenshot failed: {error}"));
                (None, None)
            }
        }
    } else {
        (None, None)
    };

    Ok(ScreenshotCapture {
        desktop_png,
        desktop_dark_png,
        thumbnail_png,
        thumbnail_dark_png,
        warnings,
    })
}

async fn capture_single_screenshot(
    url: &str,
    viewport: Viewport,
    variant: ScreenshotVariant,
) -> Result<Vec<u8>, String> {
    let screenshot_path = screenshot_temp_path();
    let window_size = format!("{},{}", viewport.width, viewport.height);

    let mut command = Command::new(screenshot_chromium_path());
    command
        .arg("--headless=new")
        .arg("--disable-gpu")
        .arg("--hide-scrollbars")
        .arg("--run-all-compositor-stages-before-draw")
        .arg(format!(
            "--virtual-time-budget={}",
            screenshot_render_wait_ms()
        ))
        .arg(format!("--window-size={window_size}"))
        .arg(format!("--screenshot={}", screenshot_path.display()))
        .arg(url);
    if matches!(variant, ScreenshotVariant::Dark) {
        command
            .arg("--force-dark-mode")
            .arg("--enable-features=WebContentsForceDark");
    }

    let timeout = screenshot_timeout();
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| format!("screenshot capture timed out after {}s", timeout.as_secs()))?
        .map_err(|err| format!("failed to launch chromium for screenshot: {err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let _ = tokio::fs::remove_file(&screenshot_path).await;
        return Err(format!(
            "chromium exited with status {}: stderr={}, stdout={}",
            output.status, stderr, stdout
        ));
    }

    let bytes = tokio::fs::read(&screenshot_path)
        .await
        .map_err(|err| format!("failed to read screenshot file {screenshot_path:?}: {err}"))?;
    let _ = tokio::fs::remove_file(&screenshot_path).await;

    if bytes.is_empty() {
        return Err("chromium created an empty screenshot payload".to_string());
    }

    Ok(bytes)
}

fn screenshot_temp_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let jitter = jitter_ms();
    std::env::temp_dir().join(format!("hyperlinked-screenshot-{nanos:x}-{jitter:x}.png"))
}

fn screenshot_chromium_path() -> String {
    chromium_path()
}

fn snapshot_content_chromium_path() -> String {
    chromium_path()
}

fn chromium_path() -> String {
    std::env::var(CHROMIUM_PATH_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "chromium".to_string())
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

fn snapshot_content_use_chromium() -> bool {
    env_bool("SNAPSHOT_CONTENT_USE_CHROMIUM", true)
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

fn screenshot_dark_mode_enabled() -> bool {
    env_bool("SCREENSHOT_DARK_MODE_ENABLED", true)
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

fn build_square_thumbnail(source_png: &[u8], size: u32) -> Result<Vec<u8>, String> {
    let image = image::load_from_memory(source_png)
        .map_err(|err| format!("invalid screenshot png: {err}"))?;
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return Err("invalid screenshot dimensions".to_string());
    }

    let side = width.min(height);
    let x = (width.saturating_sub(side)) / 2;
    let y = 0;
    let square = image.crop_imm(x, y, side, side);
    let thumbnail = square.resize_exact(size, size, FilterType::Lanczos3);

    let mut buffer = Cursor::new(Vec::new());
    thumbnail
        .write_to(&mut buffer, image::ImageFormat::Png)
        .map_err(|err| format!("failed to encode screenshot thumbnail png: {err}"))?;
    Ok(buffer.into_inner())
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
    let jitter = jitter_ms();
    Duration::from_millis(base.saturating_add(jitter))
}

fn jitter_ms() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos() as u64)
        .unwrap_or(0);
    nanos % (RETRY_JITTER_MAX_MS + 1)
}

fn format_retry_failure(error: &RetryFailure) -> String {
    format!(
        "{} (attempts={})",
        error.final_error.message,
        error.attempts.len()
    )
}

fn truncate_for_error_message(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let mut out = String::new();
    for _ in 0..max_chars {
        let Some(ch) = chars.next() else {
            return out;
        };
        out.push(ch);
    }
    if chars.next().is_some() {
        out.push_str("...");
    }
    out
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

fn classify_source_kind(content_type: Option<&str>, path: &str) -> SnapshotSourceKind {
    if content_type.is_some_and(is_pdf_content_type) {
        return SnapshotSourceKind::Pdf;
    }

    match content_type {
        Some(content_type) if is_html_content_type(content_type) => SnapshotSourceKind::Html,
        Some(_) if path_has_pdf_extension(path) => SnapshotSourceKind::Pdf,
        Some(_) => SnapshotSourceKind::Unsupported,
        None if path_has_pdf_extension(path) => SnapshotSourceKind::Pdf,
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

fn path_has_pdf_extension(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".pdf")
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
            classify_source_kind(Some("application/pdf; charset=binary"), "/doc"),
            SnapshotSourceKind::Pdf
        );
        assert_eq!(
            classify_source_kind(None, "/files/paper.PDF"),
            SnapshotSourceKind::Pdf
        );
        assert_eq!(
            classify_source_kind(Some("application/octet-stream"), "/files/paper.pdf"),
            SnapshotSourceKind::Pdf
        );
    }

    #[test]
    fn classifies_html_and_unsupported_sources() {
        assert_eq!(
            classify_source_kind(Some("text/html; charset=utf-8"), "/"),
            SnapshotSourceKind::Html
        );
        assert_eq!(
            classify_source_kind(Some("text/html"), "/docs/landing.pdf"),
            SnapshotSourceKind::Html
        );
        assert_eq!(
            classify_source_kind(Some("application/json"), "/api/data"),
            SnapshotSourceKind::Unsupported
        );
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
}
