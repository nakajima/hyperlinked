use std::collections::HashMap;
use std::time::Duration;

use reqwest::{Url, header::CONTENT_TYPE};
use sea_orm::ActiveValue::Set;
use sea_orm::DatabaseConnection;
use serde::Serialize;

use crate::{
    app::models::hyperlink_artifact as hyperlink_artifact_model,
    entity::{
        hyperlink,
        hyperlink_artifact::{self as hyperlink_artifact_entity, HyperlinkArtifactKind},
    },
    processors::{
        processor::{ProcessingError, Processor},
        readability_fetch::extract_html_from_warc,
        snapshot_fetch::ensure_fetchable_url,
    },
};

const OG_META_CONTENT_TYPE: &str = "application/json";
const OG_ERROR_CONTENT_TYPE: &str = "application/json";
const OG_IMAGE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(8);
const OG_IMAGE_DOWNLOAD_MAX_BYTES: usize = 8 * 1024 * 1024;

pub struct OgFetcher {
    job_id: i32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OgFetchOutput {
    pub meta_artifact_id: Option<i32>,
    pub image_artifact_id: Option<i32>,
    pub error_artifact_id: Option<i32>,
}

impl OgFetcher {
    pub fn new(job_id: i32) -> Self {
        Self { job_id }
    }
}

impl Processor for OgFetcher {
    type Output = OgFetchOutput;

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
                apply_selected_fields(hyperlink, &OgSelected::default());

                let payload = serde_json::to_vec_pretty(&OgMetaArtifact {
                    source_url: source_url.clone(),
                    captured_at: now_utc().to_string(),
                    source_kind: "pdf".to_string(),
                    source_artifact_id: Some(pdf_artifact.id),
                    image_artifact_id: None,
                    image_download_error: None,
                    selected: OgSelected::default(),
                    tags: Vec::new(),
                })
                .unwrap_or_else(|encode_error| {
                    format!("{{\"error\":\"failed to encode og_meta payload: {encode_error}\"}}")
                        .into_bytes()
                });

                let meta_artifact = hyperlink_artifact_model::insert(
                    connection,
                    hyperlink_id,
                    Some(self.job_id),
                    HyperlinkArtifactKind::OgMeta,
                    payload,
                    OG_META_CONTENT_TYPE,
                )
                .await
                .map_err(ProcessingError::DB)?;

                return Ok(OgFetchOutput {
                    meta_artifact_id: Some(meta_artifact.id),
                    image_artifact_id: None,
                    error_artifact_id: None,
                });
            }

            let error_artifact = persist_og_error(
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
                "og extraction requires snapshot_warc or pdf_source artifacts (error_artifact_id={})",
                error_artifact.id
            )));
        }

        let snapshot_artifact = snapshot_artifact.expect("already guarded");
        let snapshot_payload =
            hyperlink_artifact_model::load_processing_payload(&snapshot_artifact)
                .await
                .map_err(ProcessingError::DB)?;
        let html = match extract_html_from_warc(&snapshot_payload) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
            Err(error) => {
                let error_artifact = persist_og_error(
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

        let tags = extract_open_graph_tags(&html);
        let mut selected = select_open_graph_fields(&tags);
        canonicalize_og_image_url(&source_url, &mut selected);

        let (image_artifact_id, image_download_error) = match selected.image.as_deref() {
            Some(image_url) => {
                match download_og_image_artifact(
                    connection,
                    hyperlink_id,
                    self.job_id,
                    &source_url,
                    image_url,
                )
                .await
                {
                    Ok(artifact) => (Some(artifact.id), None),
                    Err(error) => {
                        tracing::warn!(
                            hyperlink_id,
                            job_id = self.job_id,
                            image_url,
                            error = %error,
                            "failed to download og:image payload"
                        );
                        (None, Some(error))
                    }
                }
            }
            None => (None, None),
        };

        apply_selected_fields(hyperlink, &selected);

        let payload = serde_json::to_vec_pretty(&OgMetaArtifact {
            source_url: source_url.clone(),
            captured_at: now_utc().to_string(),
            source_kind: "html".to_string(),
            source_artifact_id: Some(snapshot_artifact.id),
            image_artifact_id,
            image_download_error,
            selected,
            tags,
        })
        .map_err(|error| {
            ProcessingError::FetchError(format!("failed to encode og metadata payload: {error}"))
        })?;

        let meta_artifact = hyperlink_artifact_model::insert(
            connection,
            hyperlink_id,
            Some(self.job_id),
            HyperlinkArtifactKind::OgMeta,
            payload,
            OG_META_CONTENT_TYPE,
        )
        .await
        .map_err(ProcessingError::DB)?;

        Ok(OgFetchOutput {
            meta_artifact_id: Some(meta_artifact.id),
            image_artifact_id,
            error_artifact_id: None,
        })
    }
}

#[derive(Clone, Debug, Serialize)]
struct OgMetaArtifact {
    source_url: String,
    captured_at: String,
    source_kind: String,
    source_artifact_id: Option<i32>,
    image_artifact_id: Option<i32>,
    image_download_error: Option<String>,
    selected: OgSelected,
    tags: Vec<OgTag>,
}

#[derive(Clone, Debug, Default, Serialize)]
struct OgSelected {
    title: Option<String>,
    description: Option<String>,
    og_type: Option<String>,
    url: Option<String>,
    image: Option<String>,
    site_name: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct OgTag {
    property: String,
    content: String,
}

#[derive(Serialize)]
struct OgErrorArtifact {
    source_url: String,
    stage: String,
    error: String,
    failed_at: String,
}

fn extract_open_graph_tags(html: &str) -> Vec<OgTag> {
    let mut tags = Vec::new();
    let mut cursor = 0usize;
    let lowercase = html.to_ascii_lowercase();
    let bytes = html.as_bytes();

    while let Some(meta_idx) = lowercase[cursor..].find("<meta") {
        let tag_start = cursor + meta_idx;
        let Some(tag_end) = find_tag_end(bytes, tag_start + "<meta".len()) else {
            break;
        };
        if tag_end <= tag_start + "<meta".len() {
            cursor = tag_start + "<meta".len();
            continue;
        }

        let attrs = parse_html_attributes(&html[tag_start + "<meta".len()..tag_end]);
        let property = attrs
            .get("property")
            .or_else(|| attrs.get("name"))
            .map(String::as_str)
            .unwrap_or_default();
        let property = normalize_property(property);

        if !property.starts_with("og:") {
            cursor = tag_end + 1;
            continue;
        }

        let content = attrs.get("content").map(String::as_str).unwrap_or_default();
        let content = collapse_whitespace(content.trim());
        if content.is_empty() {
            cursor = tag_end + 1;
            continue;
        }

        tags.push(OgTag { property, content });
        cursor = tag_end + 1;
    }

    tags
}

fn select_open_graph_fields(tags: &[OgTag]) -> OgSelected {
    let mut first_by_property = HashMap::<&str, &str>::new();
    for tag in tags {
        first_by_property
            .entry(tag.property.as_str())
            .or_insert(tag.content.as_str());
    }

    OgSelected {
        title: first_by_property.get("og:title").map(ToString::to_string),
        description: first_by_property
            .get("og:description")
            .map(ToString::to_string),
        og_type: first_by_property.get("og:type").map(ToString::to_string),
        url: first_by_property.get("og:url").map(ToString::to_string),
        image: first_by_property.get("og:image").map(ToString::to_string),
        site_name: first_by_property
            .get("og:site_name")
            .map(ToString::to_string),
    }
}

fn apply_selected_fields(hyperlink: &mut hyperlink::ActiveModel, selected: &OgSelected) {
    hyperlink.og_title = Set(selected.title.clone());
    hyperlink.og_description = Set(selected.description.clone());
    hyperlink.og_type = Set(selected.og_type.clone());
    hyperlink.og_url = Set(selected.url.clone());
    hyperlink.og_image_url = Set(selected.image.clone());
    hyperlink.og_site_name = Set(selected.site_name.clone());
}

fn canonicalize_og_image_url(source_url: &str, selected: &mut OgSelected) {
    let Some(image_url) = selected.image.as_deref() else {
        return;
    };

    if let Ok(resolved) = resolve_og_image_url(source_url, image_url) {
        selected.image = Some(resolved.to_string());
    }
}

fn resolve_og_image_url(source_url: &str, raw_image_url: &str) -> Result<Url, String> {
    let base =
        Url::parse(source_url).map_err(|err| format!("invalid hyperlink source url: {err}"))?;
    let image_url = raw_image_url.trim();
    if image_url.is_empty() {
        return Err("og:image URL is empty".to_string());
    }
    base.join(image_url)
        .map_err(|err| format!("invalid og:image URL: {err}"))
}

async fn download_og_image_artifact(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    job_id: i32,
    source_url: &str,
    raw_image_url: &str,
) -> Result<hyperlink_artifact_entity::Model, String> {
    let image_url = resolve_og_image_url(source_url, raw_image_url)?;
    ensure_fetchable_url(&image_url).await?;

    let client = reqwest::Client::builder()
        .timeout(OG_IMAGE_DOWNLOAD_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|err| format!("failed to build og:image http client: {err}"))?;

    let mut response = client
        .get(image_url.clone())
        .send()
        .await
        .map_err(|err| format!("failed to fetch og:image URL {image_url}: {err}"))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "failed to fetch og:image URL {image_url}: status {status}"
        ));
    }

    let content_type_header = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    let mut payload = Vec::with_capacity(4096);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| format!("failed reading og:image body {image_url}: {err}"))?
    {
        let remaining = OG_IMAGE_DOWNLOAD_MAX_BYTES.saturating_sub(payload.len());
        if remaining == 0 {
            return Err(format!(
                "og:image payload exceeded {} bytes",
                OG_IMAGE_DOWNLOAD_MAX_BYTES
            ));
        }

        if chunk.len() > remaining {
            payload.extend_from_slice(&chunk[..remaining]);
            return Err(format!(
                "og:image payload exceeded {} bytes",
                OG_IMAGE_DOWNLOAD_MAX_BYTES
            ));
        }

        payload.extend_from_slice(&chunk);
    }

    if payload.is_empty() {
        return Err("og:image payload was empty".to_string());
    }

    let content_type =
        select_og_image_content_type(content_type_header.as_deref(), &payload, &image_url)
            .ok_or_else(|| "og:image payload does not look like an image".to_string())?;

    hyperlink_artifact_model::insert(
        connection,
        hyperlink_id,
        Some(job_id),
        HyperlinkArtifactKind::OgImage,
        payload,
        &content_type,
    )
    .await
    .map_err(|err| format!("failed to persist og:image artifact: {err}"))
}

fn select_og_image_content_type(header: Option<&str>, payload: &[u8], url: &Url) -> Option<String> {
    if let Some(header) = header {
        if is_image_content_type(header) {
            return Some(header.to_string());
        }
    }

    if let Some(content_type) = infer_image_content_type_from_payload(payload) {
        return Some(content_type.to_string());
    }

    infer_image_content_type_from_url(url).map(ToString::to_string)
}

fn is_image_content_type(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .map(str::trim)
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("image/"))
}

fn infer_image_content_type_from_payload(payload: &[u8]) -> Option<&'static str> {
    if payload.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if payload.len() >= 3 && payload[0] == 0xFF && payload[1] == 0xD8 && payload[2] == 0xFF {
        return Some("image/jpeg");
    }
    if payload.starts_with(b"GIF87a") || payload.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if payload.len() >= 12 && payload.starts_with(b"RIFF") && &payload[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if payload.starts_with(b"BM") {
        return Some("image/bmp");
    }
    if payload.len() >= 12
        && &payload[4..8] == b"ftyp"
        && matches!(
            &payload[8..12],
            b"avif" | b"avis" | b"heic" | b"heix" | b"heif" | b"hevc"
        )
    {
        return Some("image/avif");
    }

    let sample = String::from_utf8_lossy(&payload[..payload.len().min(256)]).to_ascii_lowercase();
    if sample.contains("<svg") {
        return Some("image/svg+xml");
    }

    None
}

fn infer_image_content_type_from_url(url: &Url) -> Option<&'static str> {
    let path = url.path().to_ascii_lowercase();
    if path.ends_with(".png") {
        Some("image/png")
    } else if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        Some("image/jpeg")
    } else if path.ends_with(".gif") {
        Some("image/gif")
    } else if path.ends_with(".webp") {
        Some("image/webp")
    } else if path.ends_with(".bmp") {
        Some("image/bmp")
    } else if path.ends_with(".svg") {
        Some("image/svg+xml")
    } else if path.ends_with(".avif") {
        Some("image/avif")
    } else {
        None
    }
}

fn normalize_property(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
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

async fn persist_og_error(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    job_id: i32,
    source_url: &str,
    stage: &str,
    error: &str,
) -> Result<hyperlink_artifact_entity::Model, sea_orm::DbErr> {
    let payload = serde_json::to_vec_pretty(&OgErrorArtifact {
        source_url: source_url.to_string(),
        stage: stage.to_string(),
        error: error.to_string(),
        failed_at: now_utc().to_string(),
    })
    .unwrap_or_else(|encode_error| {
        format!("{{\"error\":\"failed to encode og_error artifact: {encode_error}\"}}").into_bytes()
    });

    hyperlink_artifact_model::insert(
        connection,
        hyperlink_id,
        Some(job_id),
        HyperlinkArtifactKind::OgError,
        payload,
        OG_ERROR_CONTENT_TYPE,
    )
    .await
}

fn now_utc() -> sea_orm::entity::prelude::DateTime {
    sea_orm::entity::prelude::DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}
#[cfg(test)]
#[path = "../../tests/unit/processors_og_fetch.rs"]
mod tests;
