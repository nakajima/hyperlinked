use std::collections::{HashMap, HashSet};
use std::time::Duration;

use reqwest::{Url, header::CONTENT_TYPE};
use sea_orm::DatabaseConnection;
use serde::Serialize;

use crate::{
    entity::{
        hyperlink,
        hyperlink_artifact::{self as hyperlink_artifact_entity, HyperlinkArtifactKind},
    },
    model::hyperlink_artifact as hyperlink_artifact_model,
    processors::{
        processor::{ProcessingError, Processor},
        readability_fetch::extract_html_from_warc,
        snapshot_fetch::ensure_fetchable_url,
    },
};

const OEMBED_META_CONTENT_TYPE: &str = "application/json";
const OEMBED_ERROR_CONTENT_TYPE: &str = "application/json";
const OEMBED_FETCH_TIMEOUT_SECS: u64 = 12;
const MAX_OEMBED_ENDPOINTS: usize = 5;
const MAX_OEMBED_RESPONSE_BYTES: usize = 512 * 1024;
const MAX_ERROR_EXCERPT_CHARS: usize = 220;

pub struct OembedFetcher {
    job_id: i32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OembedFetchOutput {
    pub meta_artifact_id: Option<i32>,
    pub error_artifact_id: Option<i32>,
}

impl OembedFetcher {
    pub fn new(job_id: i32) -> Self {
        Self { job_id }
    }
}

impl Processor for OembedFetcher {
    type Output = OembedFetchOutput;

    async fn process<'a>(
        &'a mut self,
        hyperlink: &'a mut hyperlink::ActiveModel,
        connection: &'a DatabaseConnection,
    ) -> Result<Self::Output, ProcessingError> {
        let hyperlink_id = *hyperlink.id.as_ref();
        let source_url = hyperlink.url.as_ref().to_string();

        let snapshot_artifact = hyperlink_artifact_model::latest_for_hyperlink_kind(
            connection,
            hyperlink_id,
            HyperlinkArtifactKind::SnapshotWarc,
        )
        .await
        .map_err(ProcessingError::DB)?;

        if snapshot_artifact.is_none() {
            if let Some(pdf_artifact) = hyperlink_artifact_model::latest_for_hyperlink_kind(
                connection,
                hyperlink_id,
                HyperlinkArtifactKind::PdfSource,
            )
            .await
            .map_err(ProcessingError::DB)?
            {
                let payload = serde_json::to_vec_pretty(&OembedMetaArtifact {
                    source_url: source_url.clone(),
                    captured_at: now_utc().to_string(),
                    source_kind: "pdf".to_string(),
                    source_artifact_id: Some(pdf_artifact.id),
                    discovery: Vec::new(),
                    fetch_results: Vec::new(),
                    selected: None,
                })
                .unwrap_or_else(|encode_error| {
                    format!(
                        "{{\"error\":\"failed to encode oembed_meta payload: {encode_error}\"}}"
                    )
                    .into_bytes()
                });

                let meta_artifact = hyperlink_artifact_model::insert(
                    connection,
                    hyperlink_id,
                    Some(self.job_id),
                    HyperlinkArtifactKind::OembedMeta,
                    payload,
                    OEMBED_META_CONTENT_TYPE,
                )
                .await
                .map_err(ProcessingError::DB)?;

                return Ok(OembedFetchOutput {
                    meta_artifact_id: Some(meta_artifact.id),
                    error_artifact_id: None,
                });
            }

            let error_artifact = persist_oembed_error(
                connection,
                hyperlink_id,
                self.job_id,
                &source_url,
                "source_lookup",
                "no snapshot_warc or pdf_source artifact found for hyperlink",
            )
            .await
            .map_err(ProcessingError::DB)?;

            return Err(ProcessingError::FetchError(format!(
                "oembed extraction requires snapshot_warc or pdf_source artifacts (error_artifact_id={})",
                error_artifact.id
            )));
        }

        let snapshot_artifact = snapshot_artifact.expect("already guarded");
        let snapshot_payload = hyperlink_artifact_model::load_payload(&snapshot_artifact)
            .await
            .map_err(ProcessingError::DB)?;
        let html = match extract_html_from_warc(&snapshot_payload) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
            Err(error) => {
                let error_artifact = persist_oembed_error(
                    connection,
                    hyperlink_id,
                    self.job_id,
                    &source_url,
                    "warc_parse",
                    &error,
                )
                .await
                .map_err(ProcessingError::DB)?;
                return Err(ProcessingError::FetchError(format!(
                    "{error} (error_artifact_id={})",
                    error_artifact.id
                )));
            }
        };

        let base_url = Url::parse(&source_url).map_err(|error| {
            ProcessingError::FetchError(format!("failed to parse source url for oembed: {error}"))
        })?;
        let discovered = discover_oembed_links(&html);

        let mut discovery = Vec::with_capacity(discovered.len());
        let mut endpoints = Vec::new();
        let mut endpoint_seen = HashSet::new();
        for link in discovered {
            let resolved = match base_url.join(&link.href) {
                Ok(url) => Some(url),
                Err(_) => None,
            };

            if let Some(url) = &resolved
                && endpoint_seen.insert(url.as_str().to_string())
                && endpoints.len() < MAX_OEMBED_ENDPOINTS
            {
                endpoints.push(url.clone());
            }

            discovery.push(OembedDiscoveredLinkArtifact {
                href: link.href.clone(),
                media_type: link.media_type.clone(),
                title: link.title.clone(),
                resolved_url: resolved.as_ref().map(ToString::to_string),
                valid_endpoint: resolved.is_some(),
            });
        }

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(OEMBED_FETCH_TIMEOUT_SECS))
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .map_err(|error| {
                ProcessingError::FetchError(format!("failed to initialize oembed client: {error}"))
            })?;

        let mut fetch_results = Vec::new();
        let mut selected = None;
        for endpoint in endpoints {
            let result = fetch_oembed_endpoint(&client, endpoint).await;
            if selected.is_none()
                && let Some(value) = result.response_json.clone()
            {
                selected = Some(value);
            }
            fetch_results.push(result);
        }

        let meta_payload = serde_json::to_vec_pretty(&OembedMetaArtifact {
            source_url: source_url.clone(),
            captured_at: now_utc().to_string(),
            source_kind: "html".to_string(),
            source_artifact_id: Some(snapshot_artifact.id),
            discovery,
            fetch_results: fetch_results.clone(),
            selected,
        })
        .map_err(|error| {
            ProcessingError::FetchError(format!(
                "failed to encode oembed metadata payload: {error}"
            ))
        })?;

        let meta_artifact = hyperlink_artifact_model::insert(
            connection,
            hyperlink_id,
            Some(self.job_id),
            HyperlinkArtifactKind::OembedMeta,
            meta_payload,
            OEMBED_META_CONTENT_TYPE,
        )
        .await
        .map_err(ProcessingError::DB)?;

        let mut error_artifact_id = None;
        if !fetch_results.is_empty() && fetch_results.iter().all(|result| !result.success) {
            let error_artifact = persist_oembed_error(
                connection,
                hyperlink_id,
                self.job_id,
                &source_url,
                "endpoint_fetch",
                "discovered oembed endpoints but failed to parse a successful response",
            )
            .await
            .map_err(ProcessingError::DB)?;
            error_artifact_id = Some(error_artifact.id);
        }

        Ok(OembedFetchOutput {
            meta_artifact_id: Some(meta_artifact.id),
            error_artifact_id,
        })
    }
}

#[derive(Clone, Debug)]
struct OembedLink {
    href: String,
    media_type: Option<String>,
    title: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct OembedDiscoveredLinkArtifact {
    href: String,
    media_type: Option<String>,
    title: Option<String>,
    resolved_url: Option<String>,
    valid_endpoint: bool,
}

#[derive(Clone, Debug, Serialize)]
struct OembedFetchResultArtifact {
    endpoint: String,
    status: Option<u16>,
    content_type: Option<String>,
    success: bool,
    error: Option<String>,
    error_excerpt: Option<String>,
    response_json: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize)]
struct OembedMetaArtifact {
    source_url: String,
    captured_at: String,
    source_kind: String,
    source_artifact_id: Option<i32>,
    discovery: Vec<OembedDiscoveredLinkArtifact>,
    fetch_results: Vec<OembedFetchResultArtifact>,
    selected: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize)]
struct OembedErrorArtifact {
    source_url: String,
    stage: String,
    error: String,
    failed_at: String,
}

async fn fetch_oembed_endpoint(
    client: &reqwest::Client,
    endpoint: Url,
) -> OembedFetchResultArtifact {
    if let Err(error) = ensure_fetchable_url(&endpoint).await {
        return OembedFetchResultArtifact {
            endpoint: endpoint.to_string(),
            status: None,
            content_type: None,
            success: false,
            error: Some(format!("oembed endpoint validation failed: {error}")),
            error_excerpt: None,
            response_json: None,
        };
    }

    let response = match client
        .get(endpoint.clone())
        .header(
            reqwest::header::ACCEPT,
            "application/json, text/json;q=0.9, */*;q=0.1",
        )
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return OembedFetchResultArtifact {
                endpoint: endpoint.to_string(),
                status: None,
                content_type: None,
                success: false,
                error: Some(format!("request failed: {error}")),
                error_excerpt: None,
                response_json: None,
            };
        }
    };

    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);

    let payload = match response.bytes().await {
        Ok(payload) => payload,
        Err(error) => {
            return OembedFetchResultArtifact {
                endpoint: endpoint.to_string(),
                status: Some(status),
                content_type,
                success: false,
                error: Some(format!("failed to read response bytes: {error}")),
                error_excerpt: None,
                response_json: None,
            };
        }
    };

    if payload.len() > MAX_OEMBED_RESPONSE_BYTES {
        return OembedFetchResultArtifact {
            endpoint: endpoint.to_string(),
            status: Some(status),
            content_type,
            success: false,
            error: Some(format!(
                "response payload exceeded {} bytes",
                MAX_OEMBED_RESPONSE_BYTES
            )),
            error_excerpt: None,
            response_json: None,
        };
    }

    let excerpt = truncate(&String::from_utf8_lossy(&payload), MAX_ERROR_EXCERPT_CHARS);

    if !(200..300).contains(&status) {
        return OembedFetchResultArtifact {
            endpoint: endpoint.to_string(),
            status: Some(status),
            content_type,
            success: false,
            error: Some(format!("unexpected status code: {status}")),
            error_excerpt: Some(excerpt),
            response_json: None,
        };
    }

    match serde_json::from_slice::<serde_json::Value>(&payload) {
        Ok(response_json) => OembedFetchResultArtifact {
            endpoint: endpoint.to_string(),
            status: Some(status),
            content_type,
            success: true,
            error: None,
            error_excerpt: None,
            response_json: Some(response_json),
        },
        Err(error) => OembedFetchResultArtifact {
            endpoint: endpoint.to_string(),
            status: Some(status),
            content_type,
            success: false,
            error: Some(format!("failed to decode oembed response as json: {error}")),
            error_excerpt: Some(excerpt),
            response_json: None,
        },
    }
}

fn discover_oembed_links(html: &str) -> Vec<OembedLink> {
    let mut links = Vec::new();
    let mut seen = HashSet::new();

    for attributes in extract_link_tag_attributes(html) {
        let rel = attributes
            .get("rel")
            .map(String::as_str)
            .unwrap_or_default();
        if !rel_contains_alternate(rel) {
            continue;
        }

        let Some(href) = attributes.get("href").map(String::as_str) else {
            continue;
        };
        let href = href.trim();
        if href.is_empty() {
            continue;
        }

        let media_type = attributes
            .get("type")
            .map(|value| normalize_media_type(value));
        if !is_oembed_media_type(media_type.as_deref()) {
            continue;
        }

        if !seen.insert(href.to_string()) {
            continue;
        }

        links.push(OembedLink {
            href: href.to_string(),
            media_type,
            title: attributes
                .get("title")
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
        });
    }

    links
}

fn extract_link_tag_attributes(document: &str) -> Vec<HashMap<String, String>> {
    let bytes = document.as_bytes();
    let lowercase = document.to_ascii_lowercase();
    let mut cursor = 0usize;
    let mut attributes = Vec::new();

    while let Some(link_idx) = lowercase[cursor..].find("<link") {
        let tag_start = cursor + link_idx;
        let Some(tag_end) = find_tag_end(bytes, tag_start + "<link".len()) else {
            break;
        };
        if tag_end <= tag_start + "<link".len() {
            cursor = tag_start + "<link".len();
            continue;
        }

        let content = &document[tag_start + "<link".len()..tag_end];
        attributes.push(parse_html_attributes(content));
        cursor = tag_end + 1;
    }

    attributes
}

fn parse_html_attributes(raw: &str) -> HashMap<String, String> {
    let bytes = raw.as_bytes();
    let mut idx = 0usize;
    let mut attributes = HashMap::new();

    while idx < bytes.len() {
        idx = skip_whitespace_and_slashes(bytes, idx);
        if idx >= bytes.len() {
            break;
        }

        let name_start = idx;
        while idx < bytes.len() && is_attribute_name_char(bytes[idx]) {
            idx += 1;
        }
        if name_start == idx {
            idx += 1;
            continue;
        }

        let name = raw[name_start..idx].trim().to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }

        idx = skip_html_whitespace(bytes, idx);
        let mut value = String::new();

        if idx < bytes.len() && bytes[idx] == b'=' {
            idx += 1;
            idx = skip_html_whitespace(bytes, idx);
            if idx < bytes.len() && (bytes[idx] == b'"' || bytes[idx] == b'\'') {
                let quote = bytes[idx];
                idx += 1;
                let value_start = idx;
                while idx < bytes.len() && bytes[idx] != quote {
                    idx += 1;
                }
                value = raw[value_start..idx].to_string();
                if idx < bytes.len() {
                    idx += 1;
                }
            } else {
                let value_start = idx;
                while idx < bytes.len() && !is_html_whitespace(bytes[idx]) && bytes[idx] != b'>' {
                    idx += 1;
                }
                value = raw[value_start..idx].trim_end_matches('/').to_string();
            }
        }

        attributes.entry(name).or_insert(value);
    }

    attributes
}

fn rel_contains_alternate(rel: &str) -> bool {
    rel.split_ascii_whitespace()
        .any(|token| token.eq_ignore_ascii_case("alternate"))
}

fn normalize_media_type(value: &str) -> String {
    value
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

fn is_oembed_media_type(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        value == "application/json+oembed"
            || value == "text/json+oembed"
            || value == "application/json"
            || value == "text/json"
    })
}

fn find_tag_end(bytes: &[u8], mut idx: usize) -> Option<usize> {
    let mut quote = None;
    while idx < bytes.len() {
        let byte = bytes[idx];
        match quote {
            Some(active_quote) if byte == active_quote => quote = None,
            Some(_) => {}
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None if byte == b'>' => return Some(idx),
            None => {}
        }
        idx += 1;
    }
    None
}

fn skip_whitespace_and_slashes(bytes: &[u8], mut idx: usize) -> usize {
    while idx < bytes.len() && (is_html_whitespace(bytes[idx]) || bytes[idx] == b'/') {
        idx += 1;
    }
    idx
}

fn skip_html_whitespace(bytes: &[u8], mut idx: usize) -> usize {
    while idx < bytes.len() && is_html_whitespace(bytes[idx]) {
        idx += 1;
    }
    idx
}

fn is_html_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\n' | b'\r' | b'\t' | 0x0C)
}

fn is_attribute_name_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'-')
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in value.chars().enumerate() {
        if idx >= max_chars {
            out.push_str("...");
            break;
        }
        out.push(ch);
    }
    out
}

async fn persist_oembed_error(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    job_id: i32,
    source_url: &str,
    stage: &str,
    error: &str,
) -> Result<hyperlink_artifact_entity::Model, sea_orm::DbErr> {
    let payload = serde_json::to_vec_pretty(&OembedErrorArtifact {
        source_url: source_url.to_string(),
        stage: stage.to_string(),
        error: error.to_string(),
        failed_at: now_utc().to_string(),
    })
    .unwrap_or_else(|encode_error| {
        format!("{{\"error\":\"failed to encode oembed_error artifact: {encode_error}\"}}")
            .into_bytes()
    });

    hyperlink_artifact_model::insert(
        connection,
        hyperlink_id,
        Some(job_id),
        HyperlinkArtifactKind::OembedError,
        payload,
        OEMBED_ERROR_CONTENT_TYPE,
    )
    .await
}

fn now_utc() -> sea_orm::entity::prelude::DateTime {
    sea_orm::entity::prelude::DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_json_oembed_links() {
        let html = r#"
            <html><head>
                <link rel="alternate" type="application/json+oembed" href="https://example.com/oembed?url=1">
                <link rel="stylesheet" href="/styles.css">
                <link rel="ALTERNATE nofollow" type="application/json+oembed; charset=utf-8" href='/oembed?url=2' title="Example">
            </head></html>
        "#;

        let links = discover_oembed_links(html);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].href, "https://example.com/oembed?url=1");
        assert_eq!(links[1].href, "/oembed?url=2");
        assert_eq!(links[1].title.as_deref(), Some("Example"));
    }

    #[test]
    fn ignores_non_oembed_link_tags() {
        let html = r#"
            <link rel="alternate" type="text/html" href="/feed">
            <link rel="canonical" href="/post">
            <link rel="alternate" href="/oembed-missing-type">
        "#;

        let links = discover_oembed_links(html);
        assert!(links.is_empty());
    }

    #[test]
    fn parses_attributes_with_unquoted_values() {
        let attributes = parse_html_attributes(
            r#" rel=alternate type=application/json+oembed href=https://example.com/oembed "#,
        );
        assert_eq!(attributes.get("rel").map(String::as_str), Some("alternate"));
        assert_eq!(
            attributes.get("type").map(String::as_str),
            Some("application/json+oembed")
        );
        assert_eq!(
            attributes.get("href").map(String::as_str),
            Some("https://example.com/oembed")
        );
    }

    #[test]
    fn finds_tag_end_when_values_contain_gt_character() {
        let html = r#"<link rel="alternate" href="https://example.com/oembed?x=1>2">"#;
        let end = find_tag_end(html.as_bytes(), "<link".len()).expect("tag end should be found");
        assert_eq!(&html[end..=end], ">");
    }
}
