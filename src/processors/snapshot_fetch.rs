use reqwest::{Url, header::CONTENT_TYPE};
use sea_orm::DatabaseConnection;
use serde::Serialize;
use std::collections::HashSet;
use std::net::IpAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::{net::lookup_host, time::sleep};

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

const RETRY_ATTEMPTS: usize = 4;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const SNAPSHOT_DEADLINE: Duration = Duration::from_secs(30);
const RETRY_BASE_BACKOFF_MS: u64 = 200;
const RETRY_JITTER_MAX_MS: u64 = 125;

pub struct SnapshotFetcher {
    job_id: i32,
}

pub struct SnapshotFetchOutput {
    pub artifact_id: i32,
    pub artifact_kind: HyperlinkArtifactKind,
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

                let artifact = hyperlink_artifact::insert(
                    connection,
                    hyperlink_id,
                    Some(self.job_id),
                    kind.clone(),
                    payload,
                    &content_type,
                )
                .await
                .map_err(ProcessingError::DB)?;
                Ok(SnapshotFetchOutput {
                    artifact_id: artifact.id,
                    artifact_kind: kind,
                })
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
struct SnapshotManifest {
    source_url: String,
    captured_at: String,
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

        let resolved = match parsed.join(href.trim()) {
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
        match fetch_response_with_retry(&client, resolved, file_limit, deadline).await {
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
        source_url: parsed.to_string(),
        captured_at: now_utc().to_string(),
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
            parsed.as_str(),
            "manifest_encode",
            format!("manifest encode failed: {err}"),
            Vec::new(),
        )
    })?;
    append_record(
        &mut archive,
        "metadata",
        parsed.as_str(),
        "application/json",
        &manifest_payload,
        &[],
    );

    Ok(SnapshotCapture::Html { archive })
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

async fn ensure_fetchable_url(url: &Url) -> Result<(), String> {
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
}
