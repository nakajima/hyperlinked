use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use futures_util::{SinkExt, StreamExt};
use image::{GenericImageView, imageops::FilterType};
use reqwest::{Url, header::CONTENT_TYPE};
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::{net::lookup_host, process::Command, time::sleep};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::{
    entity::{hyperlink, hyperlink_artifact::HyperlinkArtifactKind},
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
const SCREENSHOT_CONTENT_TYPE: &str = "image/webp";
const SCREENSHOT_ERROR_CONTENT_TYPE: &str = "application/json";
const SCREENSHOT_WEBP_QUALITY: f32 = 85.0;
const SCREENSHOT_CDP_WEBP_QUALITY: u8 = 85;

const RETRY_ATTEMPTS: usize = 4;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const SNAPSHOT_DEADLINE: Duration = Duration::from_secs(30);
const RETRY_BASE_BACKOFF_MS: u64 = 200;
const RETRY_JITTER_MAX_MS: u64 = 125;
const DEFAULT_SNAPSHOT_CONTENT_TIMEOUT_SECS: u64 = 20;
const DEFAULT_SNAPSHOT_CONTENT_RENDER_WAIT_MS: u64 = 5000;
const DEFAULT_SCREENSHOT_TIMEOUT_SECS: u64 = 20;
const CHROMIUM_PATH_ENV: &str = "CHROMIUM_PATH";
const CHROMIUM_NO_SANDBOX_ENV: &str = "CHROMIUM_NO_SANDBOX";
const CHROMIUM_RUNTIME_DIR_ENV: &str = "CHROMIUM_RUNTIME_DIR";
const DEFAULT_CHROMIUM_RUNTIME_DIR: &str = "hyperlinked-chromium-runtime";
const DEFAULT_SCREENSHOT_DESKTOP_VIEWPORT: Viewport = Viewport {
    width: 1366,
    height: 4096,
};
const DEFAULT_SCREENSHOT_THUMB_SIZE: u32 = 400;
const DEFAULT_SCREENSHOT_RENDER_WAIT_MS: u64 = 5000;
const DEFAULT_SCREENSHOT_MIN_PAGE_HEIGHT: u32 = 720;
const DEFAULT_SCREENSHOT_MAX_PAGE_HEIGHT: u32 = 12_000;
const CDP_CAPTURE_RELAYOUT_WAIT_MS: u64 = 120;
const CDP_STARTUP_POLL_INTERVAL_MS: u64 = 75;

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

                let collection_settings = settings::load(connection)
                    .await
                    .map_err(ProcessingError::DB)?;
                if !collection_settings.collect_screenshots {
                    return Ok(output);
                }

                match capture_screenshots(&source_url, collection_settings.collect_screenshot_dark)
                    .await
                {
                    Ok(capture) => {
                        let screenshot_artifact = hyperlink_artifact::insert(
                            connection,
                            hyperlink_id,
                            Some(self.job_id),
                            HyperlinkArtifactKind::ScreenshotWebp,
                            capture.desktop_webp,
                            SCREENSHOT_CONTENT_TYPE,
                        )
                        .await
                        .map_err(ProcessingError::DB)?;
                        output.screenshot_artifact_id = Some(screenshot_artifact.id);

                        if let Some(dark_webp) = capture.desktop_dark_webp {
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
                            capture.thumbnail_webp,
                            SCREENSHOT_CONTENT_TYPE,
                        )
                        .await
                        .map_err(ProcessingError::DB)?;
                        output.screenshot_thumb_artifact_id = Some(thumbnail_artifact.id);

                        if let Some(dark_thumbnail_webp) = capture.thumbnail_dark_webp {
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

                        if !capture.warnings.is_empty() {
                            let font_diagnostics = current_screenshot_font_diagnostics();
                            let payload = serde_json::to_vec_pretty(&ScreenshotFailureArtifact {
                                source_url: source_url.clone(),
                                failed_at: now_utc().to_string(),
                                errors: capture.warnings,
                                chromium_path: screenshot_chromium_path(),
                                timeout_secs: screenshot_timeout().as_secs(),
                                font_diagnostics,
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
                        let font_diagnostics = current_screenshot_font_diagnostics();
                        let payload = serde_json::to_vec_pretty(&ScreenshotFailureArtifact {
                            source_url: source_url.clone(),
                            failed_at: now_utc().to_string(),
                            errors: vec![error],
                            chromium_path: screenshot_chromium_path(),
                            timeout_secs: screenshot_timeout().as_secs(),
                            font_diagnostics,
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
    desktop_webp: Vec<u8>,
    desktop_dark_webp: Option<Vec<u8>>,
    thumbnail_webp: Vec<u8>,
    thumbnail_dark_webp: Option<Vec<u8>>,
    warnings: Vec<String>,
}

#[derive(Debug)]
struct SingleScreenshotCapture {
    bytes: Vec<u8>,
    warning: Option<String>,
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

#[derive(Deserialize)]
struct ChromiumDebugTarget {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "webSocketDebuggerUrl")]
    websocket_debugger_url: Option<String>,
}

struct CdpSession {
    stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    next_id: i64,
}

impl CdpSession {
    fn new(
        stream: tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> Self {
        Self { stream, next_id: 1 }
    }

    async fn send_command(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;

        let payload = json!({
            "id": id,
            "method": method,
            "params": params
        })
        .to_string();
        self.stream
            .send(Message::Text(payload.into()))
            .await
            .map_err(|err| format!("failed to send chromium devtools command `{method}`: {err}"))?;

        loop {
            let Some(message) = self.stream.next().await else {
                return Err(format!(
                    "chromium devtools connection closed while waiting for `{method}` response"
                ));
            };

            match message {
                Ok(Message::Text(text)) => {
                    if let Some(result) = handle_cdp_text_message(id, method, text.as_ref())? {
                        return Ok(result);
                    }
                }
                Ok(Message::Binary(payload)) => {
                    let text = String::from_utf8(payload.to_vec()).map_err(|err| {
                        format!(
                            "chromium devtools sent non-utf8 binary response for `{method}`: {err}"
                        )
                    })?;
                    if let Some(result) = handle_cdp_text_message(id, method, &text)? {
                        return Ok(result);
                    }
                }
                Ok(Message::Ping(payload)) => {
                    self.stream
                        .send(Message::Pong(payload))
                        .await
                        .map_err(|err| {
                            format!("failed to respond to chromium devtools ping: {err}")
                        })?;
                }
                Ok(Message::Close(frame)) => {
                    return Err(format!(
                        "chromium devtools closed the connection while waiting for `{method}`: {frame:?}"
                    ));
                }
                Ok(_) => {}
                Err(err) => {
                    return Err(format!(
                        "failed to read chromium devtools response for `{method}`: {err}"
                    ));
                }
            }
        }
    }
}

fn handle_cdp_text_message(
    expected_id: i64,
    method: &str,
    text: &str,
) -> Result<Option<Value>, String> {
    let payload: Value = serde_json::from_str(text).map_err(|err| {
        format!("failed to decode chromium devtools response for `{method}`: {err}")
    })?;

    let Some(id) = payload.get("id").and_then(Value::as_i64).or_else(|| {
        payload
            .get("id")
            .and_then(Value::as_u64)
            .map(|value| value as i64)
    }) else {
        return Ok(None);
    };
    if id != expected_id {
        return Ok(None);
    }

    if let Some(error) = payload.get("error") {
        return Err(format!(
            "chromium devtools command `{method}` failed: {}",
            cdp_error_message(error)
        ));
    }

    Ok(Some(payload.get("result").cloned().unwrap_or(Value::Null)))
}

fn cdp_error_message(error: &Value) -> String {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    let data = error
        .get("data")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if data.is_empty() {
        message.to_string()
    } else {
        format!("{message}: {data}")
    }
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

    let runtime_dir = ensure_chromium_runtime_dir().await?;
    let mut command = Command::new(snapshot_content_chromium_path());
    configure_chromium_command(&mut command, runtime_dir.as_path());
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

async fn capture_screenshots(
    url: &str,
    collect_dark_variant: bool,
) -> Result<ScreenshotCapture, String> {
    let parsed = Url::parse(url).map_err(|err| format!("invalid screenshot url: {err}"))?;
    ensure_fetchable_url(&parsed).await?;

    let mut warnings = Vec::new();
    let desktop_viewport = screenshot_desktop_viewport();
    let desktop_capture =
        capture_single_screenshot(parsed.as_str(), desktop_viewport, ScreenshotVariant::Light)
            .await?;
    if let Some(warning) = desktop_capture.warning {
        warnings.push(format!("light screenshot fallback: {warning}"));
    }
    let desktop_webp = desktop_capture.bytes;
    let thumbnail_webp = build_square_thumbnail(&desktop_webp, screenshot_thumbnail_size())?;

    let (desktop_dark_webp, thumbnail_dark_webp) = if collect_dark_variant
        && screenshot_dark_mode_enabled()
    {
        match capture_single_screenshot(parsed.as_str(), desktop_viewport, ScreenshotVariant::Dark)
            .await
        {
            Ok(capture) => {
                if let Some(warning) = capture.warning {
                    warnings.push(format!("dark screenshot fallback: {warning}"));
                }
                let bytes = capture.bytes;
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
        desktop_webp,
        desktop_dark_webp,
        thumbnail_webp,
        thumbnail_dark_webp,
        warnings,
    })
}

async fn capture_single_screenshot(
    url: &str,
    viewport: Viewport,
    variant: ScreenshotVariant,
) -> Result<SingleScreenshotCapture, String> {
    if !screenshot_exact_height_enabled() {
        return capture_single_screenshot_fixed_viewport(url, viewport, variant)
            .await
            .map(|bytes| SingleScreenshotCapture {
                bytes,
                warning: None,
            });
    }

    match capture_single_screenshot_exact_height(url, viewport, variant).await {
        Ok(bytes) => Ok(SingleScreenshotCapture {
            bytes,
            warning: None,
        }),
        Err(exact_error) => resolve_exact_height_capture_result(
            exact_error,
            capture_single_screenshot_fixed_viewport(url, viewport, variant).await,
        ),
    }
}

fn resolve_exact_height_capture_result(
    exact_error: String,
    fallback_result: Result<Vec<u8>, String>,
) -> Result<SingleScreenshotCapture, String> {
    match fallback_result {
        Ok(bytes) => Ok(SingleScreenshotCapture {
            bytes,
            warning: Some(format!(
                "exact-height capture failed and fixed-viewport fallback was used: {exact_error}"
            )),
        }),
        Err(fallback_error) => Err(format!(
            "exact-height screenshot capture failed: {exact_error}; fixed-viewport fallback failed: {fallback_error}"
        )),
    }
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
    let debug_port = reserve_local_debug_port()?;
    let profile_dir = screenshot_profile_dir();
    tokio::fs::create_dir_all(&profile_dir)
        .await
        .map_err(|err| {
            format!(
                "failed to create temporary chromium profile {}: {err}",
                profile_dir.display()
            )
        })?;
    let runtime_dir = profile_dir.join("runtime");
    ensure_chromium_runtime_dir_at(&runtime_dir).await?;

    let mut command = Command::new(screenshot_chromium_path());
    configure_chromium_command(&mut command, runtime_dir.as_path());
    command
        .arg("--headless=new")
        .arg("--disable-gpu")
        .arg("--hide-scrollbars")
        .arg("--run-all-compositor-stages-before-draw")
        .arg(format!(
            "--virtual-time-budget={}",
            screenshot_render_wait_ms()
        ))
        .arg(format!(
            "--window-size={},{}",
            viewport.width,
            viewport.height.max(1)
        ))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg(format!("--remote-debugging-port={debug_port}"))
        .arg(format!("--user-data-dir={}", profile_dir.display()))
        .arg("about:blank");
    if matches!(variant, ScreenshotVariant::Dark) {
        command
            .arg("--force-dark-mode")
            .arg("--enable-features=WebContentsForceDark");
    }

    let mut child = command
        .spawn()
        .map_err(|err| format!("failed to launch chromium for exact-height screenshot: {err}"))?;

    let result = capture_screenshot_with_cdp(url, viewport, debug_port).await;
    let _ = cleanup_exact_height_chromium(&mut child, &profile_dir).await;
    result
}

async fn cleanup_exact_height_chromium(
    child: &mut tokio::process::Child,
    profile_dir: &PathBuf,
) -> Result<(), String> {
    match child.try_wait() {
        Ok(Some(_)) => {}
        Ok(None) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        Err(error) => {
            return Err(format!(
                "failed to inspect chromium process status: {error}"
            ));
        }
    }

    match tokio::fs::remove_dir_all(profile_dir).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "failed to clean up temporary chromium profile {}: {error}",
            profile_dir.display()
        )),
    }
}

async fn capture_screenshot_with_cdp(
    url: &str,
    viewport: Viewport,
    debug_port: u16,
) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|err| format!("failed to initialize chromium debug client: {err}"))?;
    let websocket_url = wait_for_chromium_page_websocket_url(&client, debug_port).await?;
    let (stream, _) = connect_async(websocket_url.as_str())
        .await
        .map_err(|err| format!("failed to connect to chromium devtools: {err}"))?;
    let mut cdp = CdpSession::new(stream);

    cdp.send_command("Page.enable", json!({})).await?;
    cdp.send_command("Runtime.enable", json!({})).await?;
    cdp.send_command(
        "Emulation.setDeviceMetricsOverride",
        json!({
            "width": viewport.width,
            "height": viewport.height.max(1),
            "deviceScaleFactor": 1,
            "mobile": false
        }),
    )
    .await?;
    cdp.send_command("Page.navigate", json!({ "url": url }))
        .await?;

    sleep(Duration::from_millis(screenshot_render_wait_ms())).await;

    let evaluation = cdp
        .send_command(
            "Runtime.evaluate",
            json!({
                "expression": page_height_expression(),
                "returnByValue": true
            }),
        )
        .await?;
    let page_height = clamp_page_height(
        parse_page_height_from_evaluation(&evaluation)?,
        screenshot_page_height_bounds(),
    );

    cdp.send_command(
        "Emulation.setDeviceMetricsOverride",
        json!({
            "width": viewport.width,
            "height": page_height,
            "deviceScaleFactor": 1,
            "mobile": false
        }),
    )
    .await?;
    sleep(Duration::from_millis(CDP_CAPTURE_RELAYOUT_WAIT_MS)).await;

    let screenshot = cdp
        .send_command(
            "Page.captureScreenshot",
            json!({
                "format": "webp",
                "quality": SCREENSHOT_CDP_WEBP_QUALITY,
                "fromSurface": true,
                "captureBeyondViewport": true
            }),
        )
        .await?;
    parse_webp_from_capture_screenshot_result(&screenshot)
}

fn reserve_local_debug_port() -> Result<u16, String> {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .map_err(|err| format!("failed to reserve local chromium debug port: {err}"))?;
    let port = listener
        .local_addr()
        .map_err(|err| format!("failed to read chromium debug listener address: {err}"))?
        .port();
    drop(listener);
    Ok(port)
}

async fn wait_for_chromium_page_websocket_url(
    client: &reqwest::Client,
    debug_port: u16,
) -> Result<String, String> {
    let endpoint = format!("http://127.0.0.1:{debug_port}/json/list");
    let deadline = Instant::now() + Duration::from_secs(5);

    loop {
        if Instant::now() >= deadline {
            return Err("timed out waiting for chromium devtools endpoint".to_string());
        }

        if let Ok(response) = client.get(&endpoint).send().await
            && response.status().is_success()
            && let Ok(body) = response.bytes().await
            && let Ok(targets) = serde_json::from_slice::<Vec<ChromiumDebugTarget>>(&body)
        {
            if let Some(websocket_url) = targets
                .into_iter()
                .find(|target| target.kind == "page")
                .and_then(|target| target.websocket_debugger_url)
            {
                return Ok(websocket_url);
            }
        }

        sleep(Duration::from_millis(CDP_STARTUP_POLL_INTERVAL_MS)).await;
    }
}

fn page_height_expression() -> &'static str {
    "Math.max(document.documentElement?.scrollHeight || 0, document.body?.scrollHeight || 0, document.documentElement?.offsetHeight || 0, document.body?.offsetHeight || 0, document.documentElement?.clientHeight || 0)"
}

fn parse_page_height_from_evaluation(value: &Value) -> Result<u32, String> {
    if value.get("exceptionDetails").is_some() {
        return Err("chromium page-height evaluation raised an exception".to_string());
    }

    let height = value
        .pointer("/result/value")
        .and_then(Value::as_f64)
        .ok_or_else(|| {
            "chromium page-height evaluation returned a non-numeric result".to_string()
        })?;
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

fn parse_webp_from_capture_screenshot_result(value: &Value) -> Result<Vec<u8>, String> {
    let encoded = value
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| "chromium screenshot response was missing webp data".to_string())?;
    let bytes = BASE64_STANDARD
        .decode(encoded)
        .map_err(|err| format!("failed to decode chromium screenshot payload: {err}"))?;
    if bytes.is_empty() {
        return Err("chromium screenshot response contained an empty payload".to_string());
    }
    Ok(bytes)
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

async fn capture_single_screenshot_fixed_viewport(
    url: &str,
    viewport: Viewport,
    variant: ScreenshotVariant,
) -> Result<Vec<u8>, String> {
    let screenshot_path = screenshot_temp_path();
    let window_size = format!("{},{}", viewport.width, viewport.height);
    let runtime_dir = ensure_chromium_runtime_dir().await?;

    let mut command = Command::new(screenshot_chromium_path());
    configure_chromium_command(&mut command, runtime_dir.as_path());
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

    let screenshot_bytes = tokio::fs::read(&screenshot_path)
        .await
        .map_err(|err| format!("failed to read screenshot file {screenshot_path:?}: {err}"))?;
    let _ = tokio::fs::remove_file(&screenshot_path).await;

    if screenshot_bytes.is_empty() {
        return Err("chromium created an empty screenshot payload".to_string());
    }

    encode_webp_from_image_bytes(&screenshot_bytes)
}

fn screenshot_temp_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let jitter = jitter_ms();
    // Chromium --screenshot is most reliable when targeting a .png file.
    std::env::temp_dir().join(format!("hyperlinked-screenshot-{nanos:x}-{jitter:x}.png"))
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

fn screenshot_chromium_path() -> String {
    chromium_path()
}

fn snapshot_content_chromium_path() -> String {
    chromium_path()
}

fn chromium_path() -> String {
    if let Some(path) = std::env::var(CHROMIUM_PATH_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        return path;
    }

    for candidate in chromium_binary_candidates() {
        if command_looks_available(candidate) {
            return candidate.to_string();
        }
    }

    "chromium".to_string()
}

fn chromium_binary_candidates() -> [&'static str; 5] {
    [
        "chromium",
        "google-chrome",
        "google-chrome-stable",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ]
}

fn command_looks_available(command: &str) -> bool {
    if command.contains(std::path::MAIN_SEPARATOR) {
        return Path::new(command).is_file();
    }

    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths)
            .map(|path| path.join(command))
            .any(|path| path.is_file())
    })
}

async fn ensure_chromium_runtime_dir() -> Result<PathBuf, String> {
    let runtime_dir = chromium_runtime_dir();
    ensure_chromium_runtime_dir_at(&runtime_dir).await?;
    Ok(runtime_dir)
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

fn configure_chromium_command(command: &mut Command, runtime_dir: &Path) {
    command.env("XDG_RUNTIME_DIR", runtime_dir.as_os_str());
    if chromium_no_sandbox() {
        command.arg("--no-sandbox").arg("--disable-setuid-sandbox");
    }
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

fn chromium_no_sandbox() -> bool {
    env_bool(CHROMIUM_NO_SANDBOX_ENV, process_is_root())
}

#[cfg(unix)]
fn process_is_root() -> bool {
    // Safety: `geteuid` has no preconditions and simply reports the effective uid.
    unsafe { geteuid() == 0 }
}

#[cfg(not(unix))]
fn process_is_root() -> bool {
    false
}

#[cfg(unix)]
unsafe extern "C" {
    fn geteuid() -> u32;
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

fn current_screenshot_font_diagnostics() -> Option<ScreenshotFontDiagnostics> {
    font_diagnostics::current().screenshot_artifact_context()
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
    fn exact_height_fallback_success_returns_warning() {
        let capture =
            resolve_exact_height_capture_result("cdp failed".to_string(), Ok(vec![1, 2, 3, 4]))
                .expect("fallback success should preserve screenshot bytes");

        assert_eq!(capture.bytes, vec![1, 2, 3, 4]);
        assert!(
            capture
                .warning
                .as_deref()
                .unwrap_or_default()
                .contains("fixed-viewport fallback was used")
        );
    }

    #[test]
    fn exact_height_fallback_failure_combines_errors() {
        let error = resolve_exact_height_capture_result(
            "cdp failed".to_string(),
            Err("cli failed".to_string()),
        )
        .expect_err("fallback failure should return a combined error");

        assert!(error.contains("cdp failed"));
        assert!(error.contains("cli failed"));
    }

    #[test]
    fn screenshot_failure_artifact_serializes_font_diagnostics() {
        let artifact = ScreenshotFailureArtifact {
            source_url: "https://example.com".to_string(),
            failed_at: "2026-02-26T00:00:00Z".to_string(),
            errors: vec!["screenshot failed".to_string()],
            chromium_path: "chromium".to_string(),
            timeout_secs: 20,
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
}
