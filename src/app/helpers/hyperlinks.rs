use std::collections::{HashMap, HashSet};

use reqwest::Url;
use serde::Serialize;

use crate::{
    app::models::hyperlink_title,
    entity::{
        hyperlink::{self, HyperlinkSourceType},
        hyperlink_artifact::{self, HyperlinkArtifactKind},
        hyperlink_processing_job,
    },
    server::context::Context,
};

use crate::app::controllers::hyperlinks_controller::HyperlinkResponse;

const HYPERLINKS_PATH: &str = "/hyperlinks";

pub(crate) fn to_response(
    model: &hyperlink::Model,
    latest_job: Option<&hyperlink_processing_job::Model>,
) -> HyperlinkResponse {
    HyperlinkResponse {
        id: model.id,
        title: normalize_link_title_for_display(
            model.title.as_str(),
            model.url.as_str(),
            model.raw_url.as_str(),
        ),
        url: model.url.clone(),
        raw_url: model.raw_url.clone(),
        source_type: hyperlink_source_type_name(&model.source_type).to_string(),
        clicks_count: model.clicks_count,
        last_clicked_at: model.last_clicked_at.as_ref().map(ToString::to_string),
        processing_state: processing_state_name(latest_job).to_string(),
        created_at: model.created_at.to_string(),
        updated_at: model.updated_at.to_string(),
    }
}

pub(crate) fn processing_state_name(job: Option<&hyperlink_processing_job::Model>) -> &'static str {
    match job {
        Some(job) => crate::app::models::hyperlink_processing_job::state_name(job.state.clone()),
        None => "idle",
    }
}

fn hyperlink_source_type_name(source_type: &HyperlinkSourceType) -> &'static str {
    match source_type {
        HyperlinkSourceType::Unknown => "unknown",
        HyperlinkSourceType::Html => "html",
        HyperlinkSourceType::Pdf => "pdf",
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IndexStatus {
    Processing,
    Failed,
}

pub(crate) fn index_status(
    job: Option<&hyperlink_processing_job::Model>,
    active_processing_job_ids: Option<&HashSet<i32>>,
) -> Option<IndexStatus> {
    let job = job?;
    match job.state {
        hyperlink_processing_job::HyperlinkProcessingJobState::Queued
        | hyperlink_processing_job::HyperlinkProcessingJobState::Running => {
            if let Some(active_ids) = active_processing_job_ids {
                active_ids
                    .contains(&job.id)
                    .then_some(IndexStatus::Processing)
            } else {
                Some(IndexStatus::Processing)
            }
        }
        hyperlink_processing_job::HyperlinkProcessingJobState::Failed => Some(IndexStatus::Failed),
        _ => None,
    }
}

pub(crate) fn show_path(id: i32) -> String {
    format!("{HYPERLINKS_PATH}/{id}")
}

pub(crate) async fn latest_job_optional(
    state: &Context,
    hyperlink_id: i32,
) -> Option<hyperlink_processing_job::Model> {
    crate::app::models::hyperlink_processing_job::latest_for_hyperlink(
        &state.connection,
        hyperlink_id,
    )
    .await
    .ok()
    .flatten()
}

#[derive(Serialize)]
struct HyperlinksIndexHrefQuery<'a> {
    #[serde(skip_serializing_if = "str::is_empty")]
    q: &'a str,
    page: u64,
}

pub(crate) fn hyperlinks_index_href(raw_q: &str, page: u64) -> String {
    let query = serde_urlencoded::to_string(HyperlinksIndexHrefQuery { q: raw_q, page })
        .unwrap_or_else(|_| format!("page={page}"));
    format!("{HYPERLINKS_PATH}?{query}")
}

#[derive(Clone, Debug)]
pub(crate) struct OgSummary {
    title: Option<String>,
    description: Option<String>,
    og_type: Option<String>,
    url: Option<String>,
    image_url: Option<String>,
    site_name: Option<String>,
}

impl OgSummary {
    pub(crate) fn has_values(&self) -> bool {
        self.title.is_some()
            || self.description.is_some()
            || self.og_type.is_some()
            || self.url.is_some()
            || self.image_url.is_some()
            || self.site_name.is_some()
    }

    pub(crate) fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub(crate) fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    pub(crate) fn og_type(&self) -> Option<&str> {
        self.og_type.as_deref()
    }

    pub(crate) fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    pub(crate) fn image_url(&self) -> Option<&str> {
        self.image_url.as_deref()
    }

    pub(crate) fn site_name(&self) -> Option<&str> {
        self.site_name.as_deref()
    }
}

pub(crate) fn load_og_summary(link: &hyperlink::Model) -> Option<OgSummary> {
    let summary = OgSummary {
        title: normalize_text_value(link.og_title.as_deref()),
        description: normalize_text_value(link.og_description.as_deref()),
        og_type: normalize_text_value(link.og_type.as_deref()),
        url: normalize_url_value(link.og_url.as_deref()),
        image_url: normalize_url_value(link.og_image_url.as_deref()),
        site_name: normalize_text_value(link.og_site_name.as_deref()),
    };

    summary.has_values().then_some(summary)
}

fn normalize_text_value(value: Option<&str>) -> Option<String> {
    let value = value?;
    let normalized = normalize_display_text(value);
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn normalize_url_value(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_display_text(value: &str) -> String {
    let decoded = decode_html_entities(value.trim());
    collapse_whitespace(decoded.trim())
}

pub(crate) fn normalize_link_title_for_display(title: &str, url: &str, raw_url: &str) -> String {
    let normalized = normalize_display_text(title);
    if normalized.is_empty() {
        return title.to_string();
    }

    let cleaned = hyperlink_title::strip_site_affixes(normalized.as_str(), url, raw_url);
    if cleaned.is_empty() {
        normalized
    } else {
        cleaned
    }
}

pub(crate) fn display_url_host(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let Some(parsed) = Url::parse(trimmed).ok() else {
        return trimmed.to_string();
    };
    let Some(host) = parsed.host_str() else {
        return trimmed.to_string();
    };

    let host = host.strip_prefix("www.").unwrap_or(host);
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };

    match (parsed.scheme(), parsed.port()) {
        ("http", Some(80)) | ("https", Some(443)) | (_, None) => host,
        (_, Some(port)) => format!("{host}:{port}"),
    }
}

fn decode_html_entities(value: &str) -> String {
    if !value.contains('&') {
        return value.to_string();
    }

    let mut decoded = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(entity_start_offset) = value[cursor..].find('&') {
        let amp_index = cursor + entity_start_offset;
        decoded.push_str(&value[cursor..amp_index]);

        let entity_start = amp_index + 1;
        let rest = &value[entity_start..];
        let Some(entity_end_offset) = rest.find(';') else {
            decoded.push('&');
            cursor = entity_start;
            continue;
        };

        let entity_end = entity_start + entity_end_offset;
        let entity = &value[entity_start..entity_end];
        if let Some(decoded_entity) = decode_html_entity(entity) {
            decoded.push(decoded_entity);
            cursor = entity_end + 1;
            continue;
        }

        decoded.push('&');
        cursor = entity_start;
    }

    decoded.push_str(&value[cursor..]);
    decoded
}

fn decode_html_entity(entity: &str) -> Option<char> {
    if let Some(decoded_numeric) = decode_numeric_html_entity(entity) {
        return Some(decoded_numeric);
    }

    match entity.to_ascii_lowercase().as_str() {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{00A0}'),
        _ => None,
    }
}

fn decode_numeric_html_entity(entity: &str) -> Option<char> {
    let value = if let Some(hex) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        entity.strip_prefix('#')?.parse::<u32>().ok()?
    };

    char::from_u32(value)
}

pub(crate) fn select_show_display_title(
    link: &hyperlink::Model,
    og_summary: Option<&OgSummary>,
) -> String {
    if let Some(candidate_title) = og_summary.and_then(|summary| summary.title()) {
        if metadata_title_candidate_is_usable(candidate_title)
            && should_prefer_metadata_title(
                link.title.as_str(),
                link.url.as_str(),
                link.raw_url.as_str(),
                candidate_title,
            )
        {
            return normalize_link_title_for_display(
                candidate_title,
                link.url.as_str(),
                link.raw_url.as_str(),
            );
        }
    }

    normalize_link_title_for_display(
        link.title.as_str(),
        link.url.as_str(),
        link.raw_url.as_str(),
    )
}

fn metadata_title_candidate_is_usable(candidate: &str) -> bool {
    let normalized = normalize_display_text(candidate);
    !normalized.is_empty() && normalized.chars().count() <= 200
}

fn should_prefer_metadata_title(
    current: &str,
    link_url: &str,
    raw_url: &str,
    candidate: &str,
) -> bool {
    let current_title = normalize_display_text(current);
    let candidate_title = normalize_display_text(candidate);
    let current_url_like = looks_like_url_title(&current_title, link_url, raw_url);
    let candidate_url_like = looks_like_url_title(&candidate_title, link_url, raw_url);

    if current_url_like && !candidate_url_like {
        return true;
    }

    let current_len = current_title.chars().count();
    let candidate_len = candidate_title.chars().count();
    if current_len < 12 && candidate_len >= 20 && !candidate_url_like {
        return true;
    }

    word_count(&current_title) == 1 && word_count(&candidate_title) >= 2 && !candidate_url_like
}

fn looks_like_url_title(value: &str, link_url: &str, raw_url: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return true;
    }

    if trimmed.eq_ignore_ascii_case(link_url.trim()) || trimmed.eq_ignore_ascii_case(raw_url.trim())
    {
        return true;
    }

    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn word_count(value: &str) -> usize {
    value.split_whitespace().count()
}

pub(crate) fn show_artifact_kinds() -> [HyperlinkArtifactKind; 15] {
    [
        HyperlinkArtifactKind::SnapshotWarc,
        HyperlinkArtifactKind::PdfSource,
        HyperlinkArtifactKind::PaperlessMetadata,
        HyperlinkArtifactKind::SnapshotError,
        HyperlinkArtifactKind::OgMeta,
        HyperlinkArtifactKind::OgImage,
        HyperlinkArtifactKind::OgError,
        HyperlinkArtifactKind::ScreenshotWebp,
        HyperlinkArtifactKind::ScreenshotThumbWebp,
        HyperlinkArtifactKind::ScreenshotDarkWebp,
        HyperlinkArtifactKind::ScreenshotThumbDarkWebp,
        HyperlinkArtifactKind::ScreenshotError,
        HyperlinkArtifactKind::ReadableText,
        HyperlinkArtifactKind::ReadableMeta,
        HyperlinkArtifactKind::ReadableError,
    ]
}

pub(crate) fn required_show_artifact_kinds(
    source_type: &HyperlinkSourceType,
    latest_artifacts: &HashMap<HyperlinkArtifactKind, hyperlink_artifact::Model>,
) -> Vec<HyperlinkArtifactKind> {
    let mut required = vec![
        required_source_artifact_kind(source_type, latest_artifacts),
        HyperlinkArtifactKind::OgMeta,
        HyperlinkArtifactKind::ReadableText,
        HyperlinkArtifactKind::ReadableMeta,
    ];

    required.extend(required_screenshot_artifact_kinds(
        source_type,
        latest_artifacts,
    ));

    required
}

fn required_source_artifact_kind(
    source_type: &HyperlinkSourceType,
    latest_artifacts: &HashMap<HyperlinkArtifactKind, hyperlink_artifact::Model>,
) -> HyperlinkArtifactKind {
    match source_type {
        HyperlinkSourceType::Pdf => HyperlinkArtifactKind::PdfSource,
        HyperlinkSourceType::Html => HyperlinkArtifactKind::SnapshotWarc,
        HyperlinkSourceType::Unknown => latest_source_artifact_kind(latest_artifacts)
            .unwrap_or(HyperlinkArtifactKind::SnapshotWarc),
    }
}

fn required_screenshot_artifact_kinds(
    source_type: &HyperlinkSourceType,
    latest_artifacts: &HashMap<HyperlinkArtifactKind, hyperlink_artifact::Model>,
) -> Vec<HyperlinkArtifactKind> {
    if matches!(source_type, HyperlinkSourceType::Pdf) {
        if latest_artifacts.contains_key(&HyperlinkArtifactKind::PdfSource) {
            return vec![
                HyperlinkArtifactKind::ScreenshotThumbWebp,
                HyperlinkArtifactKind::ScreenshotThumbDarkWebp,
            ];
        }
        return Vec::new();
    }

    vec![
        HyperlinkArtifactKind::ScreenshotWebp,
        HyperlinkArtifactKind::ScreenshotDarkWebp,
        HyperlinkArtifactKind::ScreenshotThumbWebp,
        HyperlinkArtifactKind::ScreenshotThumbDarkWebp,
    ]
}

fn latest_source_artifact_kind(
    latest_artifacts: &HashMap<HyperlinkArtifactKind, hyperlink_artifact::Model>,
) -> Option<HyperlinkArtifactKind> {
    let snapshot = latest_artifacts.get(&HyperlinkArtifactKind::SnapshotWarc);
    let pdf = latest_artifacts.get(&HyperlinkArtifactKind::PdfSource);

    match (snapshot, pdf) {
        (Some(snapshot), Some(pdf)) => {
            if artifact_is_newer(pdf, snapshot) {
                Some(HyperlinkArtifactKind::PdfSource)
            } else {
                Some(HyperlinkArtifactKind::SnapshotWarc)
            }
        }
        (Some(_), None) => Some(HyperlinkArtifactKind::SnapshotWarc),
        (None, Some(_)) => Some(HyperlinkArtifactKind::PdfSource),
        (None, None) => None,
    }
}

fn artifact_is_newer(
    candidate: &hyperlink_artifact::Model,
    current: &hyperlink_artifact::Model,
) -> bool {
    candidate.created_at > current.created_at
        || (candidate.created_at == current.created_at && candidate.id > current.id)
}

fn artifact_kind_info(
    kind: &HyperlinkArtifactKind,
) -> (&'static str, &'static str, &'static str, bool) {
    match kind {
        HyperlinkArtifactKind::SnapshotWarc => ("snapshot_warc", "Snapshot WARC", "warc", false),
        HyperlinkArtifactKind::PdfSource => ("pdf_source", "PDF Source", "pdf", false),
        HyperlinkArtifactKind::PaperlessMetadata => {
            ("paperless_metadata", "Paperless Metadata", "json", false)
        }
        HyperlinkArtifactKind::SnapshotError => ("snapshot_error", "Snapshot Error", "json", true),
        HyperlinkArtifactKind::OembedMeta => ("oembed_meta", "oEmbed Metadata", "json", false),
        HyperlinkArtifactKind::OembedError => ("oembed_error", "oEmbed Error", "json", true),
        HyperlinkArtifactKind::OgMeta => ("og_meta", "Open Graph Metadata", "json", false),
        HyperlinkArtifactKind::OgImage => ("og_image", "Open Graph Image", "img", false),
        HyperlinkArtifactKind::OgError => ("og_error", "Open Graph Error", "json", true),
        HyperlinkArtifactKind::ReadableText => ("readable_text", "Readable Markdown", "md", false),
        HyperlinkArtifactKind::ReadableMeta => {
            ("readable_meta", "Readable Metadata", "json", false)
        }
        HyperlinkArtifactKind::ReadableError => ("readable_error", "Readable Error", "json", true),
        HyperlinkArtifactKind::ScreenshotWebp => {
            ("screenshot_webp", "Screenshot WebP", "webp", false)
        }
        HyperlinkArtifactKind::ScreenshotThumbWebp => (
            "screenshot_thumb_webp",
            "Screenshot Thumbnail",
            "webp",
            false,
        ),
        HyperlinkArtifactKind::ScreenshotDarkWebp => {
            ("screenshot_dark_webp", "Screenshot Dark", "webp", false)
        }
        HyperlinkArtifactKind::ScreenshotThumbDarkWebp => (
            "screenshot_thumb_dark_webp",
            "Screenshot Thumbnail Dark",
            "webp",
            false,
        ),
        HyperlinkArtifactKind::ScreenshotError => {
            ("screenshot_error", "Screenshot Error", "json", true)
        }
    }
}

pub(crate) fn parse_artifact_kind(value: &str) -> Option<HyperlinkArtifactKind> {
    show_artifact_kinds()
        .into_iter()
        .find(|kind| artifact_kind_info(kind).0 == value)
}

pub(crate) fn artifact_kind_slug(kind: &HyperlinkArtifactKind) -> &'static str {
    artifact_kind_info(kind).0
}

pub(crate) fn artifact_kind_label(kind: &HyperlinkArtifactKind) -> &'static str {
    artifact_kind_info(kind).1
}

pub(crate) fn artifact_fetch_dependency_label(
    kind: &hyperlink_processing_job::HyperlinkProcessingJobKind,
) -> &'static str {
    match kind {
        hyperlink_processing_job::HyperlinkProcessingJobKind::Snapshot => "source artifacts",
        hyperlink_processing_job::HyperlinkProcessingJobKind::Og => "Open Graph metadata",
        hyperlink_processing_job::HyperlinkProcessingJobKind::Readability => {
            "readability artifacts"
        }
        hyperlink_processing_job::HyperlinkProcessingJobKind::Oembed => "oEmbed metadata",
        hyperlink_processing_job::HyperlinkProcessingJobKind::SublinkDiscovery => {
            "sublink discovery"
        }
    }
}

fn artifact_kind_file_extension(kind: &HyperlinkArtifactKind) -> &'static str {
    artifact_kind_info(kind).2
}

pub(crate) fn artifact_download_file_extension(
    kind: &HyperlinkArtifactKind,
    artifact: &hyperlink_artifact::Model,
) -> String {
    if *kind == HyperlinkArtifactKind::SnapshotWarc
        && crate::app::models::hyperlink_artifact::is_snapshot_warc_gzip_artifact(artifact)
    {
        return "warc.gz".to_string();
    }

    artifact_kind_file_extension(kind).to_string()
}

pub(crate) fn artifact_download_path(hyperlink_id: i32, kind: &HyperlinkArtifactKind) -> String {
    format!(
        "{HYPERLINKS_PATH}/{hyperlink_id}/artifacts/{}",
        artifact_kind_slug(kind)
    )
}

pub(crate) fn artifact_inline_path(hyperlink_id: i32, kind: &HyperlinkArtifactKind) -> String {
    format!(
        "{HYPERLINKS_PATH}/{hyperlink_id}/artifacts/{}/inline",
        artifact_kind_slug(kind)
    )
}

pub(crate) fn artifact_pdf_preview_path(hyperlink_id: i32) -> String {
    format!("{HYPERLINKS_PATH}/{hyperlink_id}/artifacts/pdf_source/preview")
}

pub(crate) fn artifact_delete_path(hyperlink_id: i32, kind: &HyperlinkArtifactKind) -> String {
    format!(
        "{HYPERLINKS_PATH}/{hyperlink_id}/artifacts/{}/delete",
        artifact_kind_slug(kind)
    )
}

pub(crate) fn artifact_fetch_path(hyperlink_id: i32, kind: &HyperlinkArtifactKind) -> String {
    format!(
        "{HYPERLINKS_PATH}/{hyperlink_id}/artifacts/{}/fetch",
        artifact_kind_slug(kind)
    )
}

pub(crate) fn is_readability_artifact_kind(kind: &HyperlinkArtifactKind) -> bool {
    matches!(
        kind,
        HyperlinkArtifactKind::ReadableText
            | HyperlinkArtifactKind::ReadableMeta
            | HyperlinkArtifactKind::ReadableError
    )
}

pub(crate) fn render_relative_time(datetime: &sea_orm::entity::prelude::DateTime) -> String {
    let datetime_iso = datetime.format("%Y-%m-%dT%H:%M:%SZ");
    let datetime_human = datetime.format("%b %d, %Y %H:%M UTC");
    format!("<relative-time datetime=\"{datetime_iso}\">{datetime_human}</relative-time>")
}

pub(crate) fn format_size_bytes(size_bytes: i32) -> String {
    let bytes = size_bytes.max(0) as f64;
    if bytes < 1024.0 {
        return format!("{}B", bytes as i64);
    }
    if bytes < 1024.0 * 1024.0 {
        return format!("{:.1}KB", bytes / 1024.0);
    }
    format!("{:.1}MB", bytes / (1024.0 * 1024.0))
}
