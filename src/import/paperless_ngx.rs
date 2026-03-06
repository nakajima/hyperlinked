use chrono::{DateTime as ChronoDateTime, NaiveDate, Utc};
use reqwest::{Url, header};
use sea_orm::{
    ActiveModelTrait,
    ActiveValue::Set,
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    entity::prelude::{DateTime, DateTimeUtc},
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::{
    entity::{
        hyperlink,
        hyperlink_artifact::{self, HyperlinkArtifactKind},
        hyperlink_processing_job::HyperlinkProcessingJobKind,
    },
    model::{
        hyperlink_artifact as hyperlink_artifact_model, hyperlink_processing_job, settings,
        tagging_settings,
    },
};

const PDF_CONTENT_TYPE: &str = "application/pdf";
const PDF_SIGNATURE: &[u8] = b"%PDF-";
const UPLOADS_PREFIX: &str = "/uploads";
const DEFAULT_FILENAME: &str = "document.pdf";
const DEFAULT_TITLE: &str = "Untitled PDF";
const REQUEST_TIMEOUT_SECS: u64 = 30;
const MAX_REDIRECT_HOPS: usize = 8;

#[derive(Clone, Debug)]
pub struct ImportOptions {
    pub base_url: String,
    pub api_token: String,
    pub since: Option<ChronoDateTime<Utc>>,
    pub page_size: Option<usize>,
    pub dry_run: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportFailure {
    pub document_id: Option<i64>,
    pub message: String,
    pub document_json: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ImportSummary {
    pub scanned: usize,
    pub imported: usize,
    pub skipped_duplicate: usize,
    pub skipped_non_pdf: usize,
    pub skipped_before_since: usize,
    pub failed: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ImportReport {
    pub summary: ImportSummary,
    pub failures: Vec<ImportFailure>,
}

struct DocumentsPage {
    next: Option<String>,
    results: Vec<Value>,
}

struct PaperlessApi {
    client: reqwest::Client,
    authorization_value: String,
    api_root_url: Url,
    documents_url: Url,
    page_size: Option<usize>,
}

impl PaperlessApi {
    fn new(options: &ImportOptions) -> Result<Self, String> {
        let api_token = options.api_token.trim();
        if api_token.is_empty() {
            return Err("paperless token is required".to_string());
        }

        let base_url = parse_base_url(&options.base_url)?;
        let api_root_url = api_root_url(&base_url)?;
        let documents_url = api_root_url
            .join("documents/")
            .map_err(|err| format!("failed to build documents endpoint: {err}"))?;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|err| format!("failed to build paperless api client: {err}"))?;

        Ok(Self {
            client,
            authorization_value: format!("Token {api_token}"),
            api_root_url,
            documents_url,
            page_size: options.page_size,
        })
    }

    fn first_documents_url(&self) -> Url {
        let mut url = self.documents_url.clone();
        if let Some(page_size) = self.page_size {
            url.query_pairs_mut()
                .append_pair("page_size", &page_size.to_string());
        }
        url
    }

    fn resolve_next_url(
        &self,
        current_url: &Url,
        next: Option<&str>,
    ) -> Result<Option<Url>, String> {
        let Some(next) = next.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(None);
        };

        if let Ok(url) = Url::parse(next) {
            return Ok(Some(url));
        }

        current_url
            .join(next)
            .map(Some)
            .map_err(|err| format!("failed to parse next page url '{next}': {err}"))
    }

    async fn fetch_documents_page(&self, page_url: &Url) -> Result<DocumentsPage, String> {
        let response = self
            .send_get_with_auth_follow_redirects(page_url.clone())
            .await
            .map_err(|err| format!("failed to fetch paperless documents page {page_url}: {err}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(format!(
                "paperless documents request failed for {page_url}: status {} ({})",
                status,
                summarize_http_body(&body)
            ));
        }

        let body_text = response
            .text()
            .await
            .map_err(|err| format!("failed to read paperless documents response body: {err}"))?;
        let value: Value = serde_json::from_str(&body_text)
            .map_err(|err| format!("failed to parse paperless documents response json: {err}"))?;

        if let Some(results) = value.get("results").and_then(Value::as_array) {
            return Ok(DocumentsPage {
                next: value
                    .get("next")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                results: results.clone(),
            });
        }

        if let Some(results) = value.as_array() {
            return Ok(DocumentsPage {
                next: None,
                results: results.clone(),
            });
        }

        Err("paperless documents response did not include a results array".to_string())
    }

    fn download_url_for_document(
        &self,
        document_id: i64,
        document: &Map<String, Value>,
    ) -> Result<Url, String> {
        if let Some(raw) =
            first_string_value(document, &["download_url", "download", "download_link"])
        {
            if let Ok(url) = Url::parse(raw.as_str()) {
                return Ok(url);
            }

            return self.resolve_relative_url(&raw);
        }

        self.api_root_url
            .join(&format!("documents/{document_id}/download/"))
            .map_err(|err| {
                format!(
                    "failed to build paperless document download endpoint for id {document_id}: {err}"
                )
            })
    }

    fn resolve_relative_url(&self, raw: &str) -> Result<Url, String> {
        if raw.starts_with('/') {
            let mut origin = self.api_root_url.clone();
            origin.set_path("/");
            origin.set_query(None);
            origin.set_fragment(None);
            return origin.join(raw).map_err(|err| {
                format!("failed to parse paperless document download url '{raw}': {err}")
            });
        }

        self.api_root_url.join(raw).map_err(|err| {
            format!("failed to parse paperless document download url '{raw}': {err}")
        })
    }

    async fn download_document(
        &self,
        download_url: &Url,
    ) -> Result<(Vec<u8>, Option<String>), String> {
        let response = self
            .send_get_with_auth_follow_redirects(download_url.clone())
            .await
            .map_err(|err| {
                format!("failed to download paperless document {download_url}: {err}")
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(format!(
                "paperless download request failed for {download_url}: status {} ({})",
                status,
                summarize_http_body(&body)
            ));
        }

        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
        let payload = response
            .bytes()
            .await
            .map_err(|err| format!("failed to read paperless download response body: {err}"))?
            .to_vec();
        Ok((payload, content_type))
    }

    async fn send_get_with_auth_follow_redirects(
        &self,
        initial_url: Url,
    ) -> Result<reqwest::Response, String> {
        let mut current_url = initial_url;

        for hop in 0..=MAX_REDIRECT_HOPS {
            let response = self
                .client
                .get(current_url.clone())
                .header(header::AUTHORIZATION, self.authorization_value.clone())
                .send()
                .await
                .map_err(|err| format!("request error for {current_url}: {err}"))?;

            if !response.status().is_redirection() {
                return Ok(response);
            }

            if hop == MAX_REDIRECT_HOPS {
                return Err(format!(
                    "too many redirects (>{MAX_REDIRECT_HOPS}) while requesting {current_url}"
                ));
            }

            let location = response
                .headers()
                .get(header::LOCATION)
                .ok_or_else(|| {
                    format!("redirect response from {current_url} missing location header")
                })?
                .to_str()
                .map_err(|err| {
                    format!(
                        "redirect response from {current_url} had invalid location header: {err}"
                    )
                })?;
            current_url = resolve_redirect_url(&current_url, location)?;
        }

        Err("request redirect handling reached unreachable state".to_string())
    }
}

fn resolve_redirect_url(current_url: &Url, location: &str) -> Result<Url, String> {
    Url::parse(location)
        .or_else(|_| current_url.join(location))
        .map_err(|err| format!("failed to resolve redirect location '{location}': {err}"))
}

pub async fn import_from_api(
    connection: &DatabaseConnection,
    options: ImportOptions,
    processing_queue: Option<&hyperlink_processing_job::ProcessingQueueSender>,
) -> Result<ImportReport, String> {
    let api = PaperlessApi::new(&options)?;

    let mut report = ImportReport::default();
    let mut next_url = Some(api.first_documents_url());

    while let Some(page_url) = next_url {
        let page = api.fetch_documents_page(&page_url).await?;
        report.summary.scanned += page.results.len();

        for document in page.results {
            let object = match document.as_object() {
                Some(object) => object,
                None => {
                    push_failure(
                        &mut report,
                        None,
                        "paperless document row is not an object".to_string(),
                        &document,
                    );
                    continue;
                }
            };

            let document_id = match object.get("id").and_then(Value::as_i64) {
                Some(id) => id,
                None => {
                    push_failure(
                        &mut report,
                        None,
                        "paperless document row did not include numeric id".to_string(),
                        &document,
                    );
                    continue;
                }
            };

            if let Some(since) = options.since {
                if let Some(document_timestamp) = parse_document_filter_timestamp(object) {
                    if document_timestamp < since {
                        report.summary.skipped_before_since += 1;
                        continue;
                    }
                }
            }

            let filename = sanitize_pdf_filename(
                &first_string_value(
                    object,
                    &["original_file_name", "original_filename", "filename"],
                )
                .unwrap_or_else(|| format!("paperless-document-{document_id}.pdf")),
            );
            let title = normalized_upload_title(
                first_string_value(object, &["title", "name"]).as_deref(),
                &filename,
            );
            let created_at = parse_document_created_at(object);

            let download_url = match api.download_url_for_document(document_id, object) {
                Ok(url) => url,
                Err(message) => {
                    push_failure(&mut report, Some(document_id), message, &document);
                    continue;
                }
            };

            let (payload, content_type) = match api.download_document(&download_url).await {
                Ok(download) => download,
                Err(message) => {
                    push_failure(&mut report, Some(document_id), message, &document);
                    continue;
                }
            };

            if !looks_like_pdf(&payload, content_type.as_deref()) {
                report.summary.skipped_non_pdf += 1;
                continue;
            }

            let checksum = sha256_hex(&payload);
            let is_duplicate =
                find_existing_pdf_upload(connection, checksum.as_str(), filename.as_str())
                    .await
                    .is_some();
            if is_duplicate {
                report.summary.skipped_duplicate += 1;
                continue;
            }

            if options.dry_run {
                report.summary.imported += 1;
                continue;
            }

            let metadata_payload =
                match serialize_metadata_payload(document_id, &download_url, &document) {
                    Ok(payload) => payload,
                    Err(message) => {
                        push_failure(&mut report, Some(document_id), message, &document);
                        continue;
                    }
                };

            match persist_paperless_document(
                connection,
                processing_queue,
                title,
                filename,
                payload,
                created_at,
                metadata_payload,
            )
            .await
            {
                Ok(()) => report.summary.imported += 1,
                Err(message) => {
                    push_failure(&mut report, Some(document_id), message, &document);
                    continue;
                }
            }
        }

        next_url = api.resolve_next_url(&page_url, page.next.as_deref())?;
    }

    Ok(report)
}

async fn persist_paperless_document(
    connection: &DatabaseConnection,
    processing_queue: Option<&hyperlink_processing_job::ProcessingQueueSender>,
    title: String,
    filename: String,
    payload: Vec<u8>,
    created_at: Option<DateTime>,
    metadata_payload: Vec<u8>,
) -> Result<(), String> {
    let now = now_utc();
    let created_at = created_at.unwrap_or(now);
    let placeholder_url = pending_upload_placeholder(filename.as_str());

    let inserted = (hyperlink::ActiveModel {
        title: Set(title),
        url: Set(placeholder_url.clone()),
        raw_url: Set(placeholder_url),
        discovery_depth: Set(crate::model::hyperlink::ROOT_DISCOVERY_DEPTH),
        clicks_count: Set(0),
        created_at: Set(created_at),
        updated_at: Set(now),
        ..Default::default()
    })
    .insert(connection)
    .await
    .map_err(|err| format!("failed to insert paperless hyperlink: {err}"))?;

    let mut active: hyperlink::ActiveModel = inserted.into();
    let final_url = upload_hyperlink_url(*active.id.as_ref(), filename.as_str());
    active.url = Set(final_url.clone());
    active.raw_url = Set(final_url);
    active.updated_at = Set(now_utc());
    let updated = active
        .update(connection)
        .await
        .map_err(|err| format!("failed to finalize paperless hyperlink url: {err}"))?;

    hyperlink_artifact_model::insert(
        connection,
        updated.id,
        None,
        HyperlinkArtifactKind::PdfSource,
        payload,
        PDF_CONTENT_TYPE,
    )
    .await
    .map_err(|err| format!("failed to persist paperless pdf artifact: {err}"))?;

    hyperlink_artifact_model::insert(
        connection,
        updated.id,
        None,
        HyperlinkArtifactKind::PaperlessMetadata,
        metadata_payload,
        "application/json",
    )
    .await
    .map_err(|err| format!("failed to persist paperless metadata artifact: {err}"))?;

    enqueue_processing_jobs(connection, processing_queue, updated.id).await?;

    Ok(())
}

async fn enqueue_processing_jobs(
    connection: &DatabaseConnection,
    processing_queue: Option<&hyperlink_processing_job::ProcessingQueueSender>,
    hyperlink_id: i32,
) -> Result<(), String> {
    let Some(queue) = processing_queue else {
        return Ok(());
    };

    let collection_settings = settings::load(connection)
        .await
        .map_err(|err| format!("failed to load artifact collection settings: {err}"))?;

    if collection_settings.collect_og {
        hyperlink_processing_job::enqueue_for_hyperlink_kind(
            connection,
            hyperlink_id,
            HyperlinkProcessingJobKind::Og,
            Some(queue),
        )
        .await
        .map_err(|err| format!("failed to enqueue og processing job: {err}"))?;
    }

    if collection_settings.collect_readability {
        hyperlink_processing_job::enqueue_for_hyperlink_kind(
            connection,
            hyperlink_id,
            HyperlinkProcessingJobKind::Readability,
            Some(queue),
        )
        .await
        .map_err(|err| format!("failed to enqueue readability processing job: {err}"))?;
    }

    let tagging_settings = tagging_settings::load(connection)
        .await
        .map_err(|err| format!("failed to load tagging settings: {err}"))?;
    if tagging_settings.classification_enabled() {
        hyperlink_processing_job::enqueue_for_hyperlink_kind(
            connection,
            hyperlink_id,
            HyperlinkProcessingJobKind::TagClassification,
            Some(queue),
        )
        .await
        .map_err(|err| format!("failed to enqueue tag classification job: {err}"))?;
    }

    Ok(())
}

fn parse_base_url(raw: &str) -> Result<Url, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("paperless base url is required".to_string());
    }

    let normalized = if trimmed.ends_with('/') {
        trimmed.to_string()
    } else {
        format!("{trimmed}/")
    };

    let url = Url::parse(&normalized)
        .map_err(|err| format!("failed to parse paperless base url '{trimmed}': {err}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("paperless base url must use http or https".to_string());
    }

    Ok(url)
}

fn api_root_url(base_url: &Url) -> Result<Url, String> {
    let base_path = base_url.path().trim_end_matches('/');
    let looks_like_api_root = base_path.ends_with("/api");
    let relative = if looks_like_api_root { "" } else { "api/" };

    base_url
        .join(relative)
        .map_err(|err| format!("failed to build paperless api root url: {err}"))
}

fn parse_document_created_at(document: &Map<String, Value>) -> Option<DateTime> {
    parse_datetime_from_keys(document, &["created", "created_date", "added", "modified"])
        .map(|value| value.naive_utc())
}

fn parse_document_filter_timestamp(document: &Map<String, Value>) -> Option<ChronoDateTime<Utc>> {
    parse_datetime_from_keys(document, &["modified", "added", "created", "created_date"])
}

fn parse_datetime_from_keys(
    object: &Map<String, Value>,
    keys: &[&str],
) -> Option<ChronoDateTime<Utc>> {
    keys.iter().find_map(|key| {
        object
            .get(*key)
            .and_then(Value::as_str)
            .and_then(parse_datetime)
    })
}

fn parse_datetime(raw: &str) -> Option<ChronoDateTime<Utc>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Ok(value) = ChronoDateTime::parse_from_rfc3339(trimmed) {
        return Some(value.with_timezone(&Utc));
    }

    const OFFSET_FORMATS: [&str; 2] = ["%Y-%m-%d %H:%M:%S%.f%:z", "%Y-%m-%dT%H:%M:%S%.f%:z"];
    for format in OFFSET_FORMATS {
        if let Ok(value) = ChronoDateTime::parse_from_str(trimmed, format) {
            return Some(value.with_timezone(&Utc));
        }
    }

    const NAIVE_FORMATS: [&str; 4] = [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
    ];

    for format in NAIVE_FORMATS {
        if let Ok(value) = chrono::NaiveDateTime::parse_from_str(trimmed, format) {
            return Some(ChronoDateTime::from_naive_utc_and_offset(value, Utc));
        }
    }

    if let Ok(value) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        if let Some(naive) = value.and_hms_opt(0, 0, 0) {
            return Some(ChronoDateTime::from_naive_utc_and_offset(naive, Utc));
        }
    }

    None
}

async fn find_existing_pdf_upload(
    connection: &DatabaseConnection,
    checksum_sha256: &str,
    filename: &str,
) -> Option<hyperlink::Model> {
    let artifacts = hyperlink_artifact::Entity::find()
        .filter(hyperlink_artifact::Column::Kind.eq(HyperlinkArtifactKind::PdfSource))
        .filter(hyperlink_artifact::Column::ChecksumSha256.eq(checksum_sha256.to_string()))
        .order_by_desc(hyperlink_artifact::Column::CreatedAt)
        .order_by_desc(hyperlink_artifact::Column::Id)
        .all(connection)
        .await
        .ok()?;

    for artifact in artifacts {
        let Some(link) = hyperlink::Entity::find_by_id(artifact.hyperlink_id)
            .one(connection)
            .await
            .ok()
            .flatten()
        else {
            continue;
        };

        if upload_filename_from_url(link.url.as_str()).as_deref() == Some(filename) {
            return Some(link);
        }
    }

    None
}

fn serialize_metadata_payload(
    document_id: i64,
    download_url: &Url,
    raw_document: &Value,
) -> Result<Vec<u8>, String> {
    let payload = json!({
        "source": "paperless_ngx",
        "document_id": document_id,
        "download_url": download_url.as_str(),
        "imported_at": Utc::now().to_rfc3339(),
        "document": raw_document,
    });

    serde_json::to_vec_pretty(&payload)
        .map_err(|err| format!("failed to serialize paperless metadata payload: {err}"))
}

fn push_failure(report: &mut ImportReport, document_id: Option<i64>, message: String, row: &Value) {
    report.summary.failed += 1;
    report.failures.push(ImportFailure {
        document_id,
        message,
        document_json: row_json(row),
    });
}

fn first_string_value(object: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        object
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

fn looks_like_pdf(payload: &[u8], content_type: Option<&str>) -> bool {
    if payload.len() >= PDF_SIGNATURE.len() && &payload[..PDF_SIGNATURE.len()] == PDF_SIGNATURE {
        return true;
    }

    content_type
        .map(str::to_ascii_lowercase)
        .is_some_and(|value| value.contains(PDF_CONTENT_TYPE))
}

fn normalized_upload_title(raw_title: Option<&str>, filename: &str) -> String {
    if let Some(raw_title) = raw_title {
        let trimmed = raw_title.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    filename
        .strip_suffix(".pdf")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_TITLE)
        .to_string()
}

fn sanitize_pdf_filename(raw: &str) -> String {
    let trimmed = raw.trim();
    let without_query = trimmed.split(['?', '#']).next().unwrap_or(trimmed);
    let last_component = without_query
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(without_query);
    let mut cleaned = last_component
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | ' ' | '(' | ')'))
        .collect::<String>();

    cleaned = cleaned.trim().trim_matches('.').to_string();
    if cleaned.is_empty() {
        cleaned = DEFAULT_FILENAME.to_string();
    }

    if !cleaned.to_ascii_lowercase().ends_with(".pdf") {
        cleaned.push_str(".pdf");
    }

    while cleaned.contains("..") {
        cleaned = cleaned.replace("..", ".");
    }

    cleaned
}

fn upload_hyperlink_url(id: i32, filename: &str) -> String {
    format!("{UPLOADS_PREFIX}/{id}/{filename}")
}

fn pending_upload_placeholder(filename: &str) -> String {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{UPLOADS_PREFIX}/pending-{nonce}/{filename}")
}

fn upload_filename_from_url(url: &str) -> Option<String> {
    let path = if url.starts_with('/') {
        url.split(['?', '#']).next().unwrap_or(url).to_string()
    } else {
        let parsed = Url::parse(url).ok()?;
        parsed.path().to_string()
    };

    let mut parts = path.trim_start_matches('/').split('/');
    if parts.next()? != "uploads" {
        return None;
    }
    let _id = parts.next()?;
    let filename = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    Some(filename.to_string())
}

fn sha256_hex(payload: &[u8]) -> String {
    let digest = Sha256::digest(payload);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(hex_char((byte >> 4) & 0x0F));
        output.push(hex_char(byte & 0x0F));
    }
    output
}

fn hex_char(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        10..=15 => (b'a' + (value - 10)) as char,
        _ => unreachable!("hex nibble must be in range 0..=15"),
    }
}

fn row_json(row: &Value) -> String {
    serde_json::to_string_pretty(row).unwrap_or_else(|_| row.to_string())
}

fn summarize_http_body(body: &str) -> String {
    let compact = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return "empty response".to_string();
    }

    if compact.chars().count() > 240 {
        let clipped: String = compact.chars().take(240).collect();
        return format!("{clipped}...");
    }

    compact
}

fn now_utc() -> DateTime {
    DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::{Path, Query, State},
        http::{HeaderValue, StatusCode, header},
        response::{IntoResponse, Redirect},
        routing::get,
    };
    use sea_orm::EntityTrait;
    use std::{collections::HashMap, sync::Arc};

    #[derive(Clone)]
    struct MockPaperlessState {
        pages: Arc<Vec<Vec<Value>>>,
        downloads: Arc<HashMap<i64, (Vec<u8>, String)>>,
    }

    #[derive(Clone)]
    struct RedirectState {
        redirected_page_url: String,
        redirected_download_base_url: String,
    }

    async fn list_documents(
        State(state): State<MockPaperlessState>,
        Query(params): Query<HashMap<String, String>>,
    ) -> Json<Value> {
        let page = params
            .get("page")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1)
            .max(1);
        let index = page.saturating_sub(1);

        let results = state.pages.get(index).cloned().unwrap_or_default();
        let next = if index + 1 < state.pages.len() {
            Some(format!("/api/documents/?page={}", page + 1))
        } else {
            None
        };

        Json(json!({
            "count": state.pages.iter().map(Vec::len).sum::<usize>(),
            "next": next,
            "previous": if page > 1 { Some(format!("/api/documents/?page={}", page - 1)) } else { None },
            "results": results,
        }))
    }

    async fn download_document(
        Path(id): Path<i64>,
        State(state): State<MockPaperlessState>,
    ) -> (StatusCode, [(header::HeaderName, HeaderValue); 1], Vec<u8>) {
        let Some((payload, content_type)) = state.downloads.get(&id).cloned() else {
            return (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"))],
                b"not found".to_vec(),
            );
        };

        (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_str(content_type.as_str())
                    .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
            )],
            payload,
        )
    }

    async fn list_documents_with_redirect(
        Query(params): Query<HashMap<String, String>>,
    ) -> Json<Value> {
        let page = params
            .get("page")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1)
            .max(1);

        if page == 1 {
            return Json(json!({
                "count": 1,
                "next": "/api/documents/page-2-redirect/",
                "previous": null,
                "results": [],
            }));
        }

        Json(json!({
            "count": 1,
            "next": null,
            "previous": "/api/documents/?page=1",
            "results": [],
        }))
    }

    async fn page_2_redirect(State(state): State<RedirectState>) -> Redirect {
        Redirect::temporary(&state.redirected_page_url)
    }

    async fn download_redirect(
        Path(id): Path<i64>,
        State(state): State<RedirectState>,
    ) -> Redirect {
        Redirect::temporary(&format!(
            "{}/api/documents/{id}/download/",
            state.redirected_download_base_url
        ))
    }

    async fn list_documents_auth_required(
        State(state): State<MockPaperlessState>,
        Query(params): Query<HashMap<String, String>>,
        headers: axum::http::HeaderMap,
    ) -> impl IntoResponse {
        let auth_value = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok());
        if auth_value != Some("Token paperless-token") {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "detail": "Authentication credentials were not provided." })),
            )
                .into_response();
        }

        let page = params
            .get("page")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1)
            .max(1);
        let index = page.saturating_sub(1);
        let results = state.pages.get(index).cloned().unwrap_or_default();

        Json(json!({
            "count": state.pages.iter().map(Vec::len).sum::<usize>(),
            "next": null,
            "previous": null,
            "results": results,
        }))
        .into_response()
    }

    async fn download_document_auth_required(
        Path(id): Path<i64>,
        State(state): State<MockPaperlessState>,
        headers: axum::http::HeaderMap,
    ) -> impl IntoResponse {
        let auth_value = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok());
        if auth_value != Some("Token paperless-token") {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "detail": "Authentication credentials were not provided." })),
            )
                .into_response();
        }

        let Some((payload, content_type)) = state.downloads.get(&id).cloned() else {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "detail": "Document not found." })),
            )
                .into_response();
        };

        (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_str(content_type.as_str())
                    .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
            )],
            payload,
        )
            .into_response()
    }

    async fn start_mock_paperless(
        pages: Vec<Vec<Value>>,
        downloads: HashMap<i64, (Vec<u8>, String)>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route("/api/documents/", get(list_documents))
            .route("/api/documents/{id}/download/", get(download_document))
            .with_state(MockPaperlessState {
                pages: Arc::new(pages),
                downloads: Arc::new(downloads),
            });

        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("listener should bind");
        let addr = listener
            .local_addr()
            .expect("listener should have local addr");

        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock paperless server should run");
        });

        (format!("http://{addr}"), handle)
    }

    async fn start_auth_required_paperless(
        pages: Vec<Vec<Value>>,
        downloads: HashMap<i64, (Vec<u8>, String)>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route("/api/documents/", get(list_documents_auth_required))
            .route(
                "/api/documents/{id}/download/",
                get(download_document_auth_required),
            )
            .with_state(MockPaperlessState {
                pages: Arc::new(pages),
                downloads: Arc::new(downloads),
            });

        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("listener should bind");
        let addr = listener
            .local_addr()
            .expect("listener should have local addr");

        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock paperless server should run");
        });

        (format!("http://{addr}"), handle)
    }

    async fn start_redirecting_front_server(
        redirected_page_url: String,
        redirected_download_base_url: String,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route("/api/documents/", get(list_documents_with_redirect))
            .route("/api/documents/page-2-redirect/", get(page_2_redirect))
            .route("/api/documents/{id}/download/", get(download_redirect))
            .with_state(RedirectState {
                redirected_page_url,
                redirected_download_base_url,
            });

        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("listener should bind");
        let addr = listener
            .local_addr()
            .expect("listener should have local addr");

        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock paperless server should run");
        });

        (format!("http://{addr}"), handle)
    }

    async fn new_connection() -> DatabaseConnection {
        let connection = crate::server::test_support::new_memory_connection().await;
        crate::server::test_support::initialize_hyperlinks_schema(&connection).await;
        connection
    }

    #[tokio::test]
    async fn imports_pdf_and_stores_metadata_artifact() {
        let connection = new_connection().await;

        let pages = vec![vec![json!({
            "id": 101,
            "title": "RFC 9114",
            "created": "2026-01-14T20:11:03Z",
            "original_file_name": "rfc-9114.pdf"
        })]];
        let mut downloads = HashMap::new();
        downloads.insert(
            101,
            (
                b"%PDF-1.7\n%imported".to_vec(),
                "application/pdf".to_string(),
            ),
        );

        let (base_url, server_task) = start_mock_paperless(pages, downloads).await;

        let report = import_from_api(
            &connection,
            ImportOptions {
                base_url,
                api_token: "paperless-token".to_string(),
                since: None,
                page_size: Some(50),
                dry_run: false,
            },
            None,
        )
        .await
        .expect("paperless import should succeed");

        server_task.abort();

        assert_eq!(report.summary.scanned, 1);
        assert_eq!(report.summary.imported, 1);
        assert_eq!(report.summary.skipped_duplicate, 0);
        assert_eq!(report.summary.skipped_non_pdf, 0);
        assert_eq!(report.summary.failed, 0);

        let links = hyperlink::Entity::find()
            .all(&connection)
            .await
            .expect("links should load");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].title, "RFC 9114");
        assert!(links[0].url.starts_with("/uploads/1/rfc-9114.pdf"));

        let artifacts = hyperlink_artifact::Entity::find()
            .all(&connection)
            .await
            .expect("artifacts should load");
        assert_eq!(artifacts.len(), 2);
        assert!(
            artifacts
                .iter()
                .any(|row| row.kind == HyperlinkArtifactKind::PdfSource)
        );
        assert!(
            artifacts
                .iter()
                .any(|row| row.kind == HyperlinkArtifactKind::PaperlessMetadata)
        );
    }

    #[tokio::test]
    async fn rerun_skips_duplicate_pdf_by_checksum_and_filename() {
        let connection = new_connection().await;

        let pages = vec![vec![json!({
            "id": 200,
            "title": "Duplicate Test",
            "original_file_name": "dup.pdf"
        })]];
        let payload = b"%PDF-1.5\n%duplicate".to_vec();
        let mut downloads = HashMap::new();
        downloads.insert(200, (payload.clone(), "application/pdf".to_string()));

        let (base_url, server_task) = start_mock_paperless(pages, downloads).await;

        let first = import_from_api(
            &connection,
            ImportOptions {
                base_url: base_url.clone(),
                api_token: "paperless-token".to_string(),
                since: None,
                page_size: None,
                dry_run: false,
            },
            None,
        )
        .await
        .expect("first import should succeed");
        assert_eq!(first.summary.imported, 1);

        let second = import_from_api(
            &connection,
            ImportOptions {
                base_url,
                api_token: "paperless-token".to_string(),
                since: None,
                page_size: None,
                dry_run: false,
            },
            None,
        )
        .await
        .expect("second import should succeed");

        server_task.abort();

        assert_eq!(second.summary.scanned, 1);
        assert_eq!(second.summary.imported, 0);
        assert_eq!(second.summary.skipped_duplicate, 1);
        assert_eq!(second.summary.failed, 0);

        let links = hyperlink::Entity::find()
            .all(&connection)
            .await
            .expect("links should load");
        assert_eq!(links.len(), 1);

        let artifacts = hyperlink_artifact::Entity::find()
            .all(&connection)
            .await
            .expect("artifacts should load");
        assert_eq!(artifacts.len(), 2);
    }

    #[tokio::test]
    async fn skips_non_pdf_downloads() {
        let connection = new_connection().await;

        let pages = vec![vec![json!({
            "id": 333,
            "title": "Not PDF",
            "original_file_name": "not-pdf.pdf"
        })]];
        let mut downloads = HashMap::new();
        downloads.insert(
            333,
            (
                b"this is plain text".to_vec(),
                "text/plain; charset=utf-8".to_string(),
            ),
        );

        let (base_url, server_task) = start_mock_paperless(pages, downloads).await;

        let report = import_from_api(
            &connection,
            ImportOptions {
                base_url,
                api_token: "paperless-token".to_string(),
                since: None,
                page_size: None,
                dry_run: false,
            },
            None,
        )
        .await
        .expect("import should succeed");

        server_task.abort();

        assert_eq!(report.summary.scanned, 1);
        assert_eq!(report.summary.imported, 0);
        assert_eq!(report.summary.skipped_non_pdf, 1);
        assert_eq!(report.summary.failed, 0);

        let links = hyperlink::Entity::find()
            .all(&connection)
            .await
            .expect("links should load");
        assert_eq!(links.len(), 0);
    }

    #[tokio::test]
    async fn handles_download_url_with_leading_slash() {
        let connection = new_connection().await;

        let pages = vec![vec![json!({
            "id": 444,
            "title": "Leading Slash Download",
            "download_url": "/api/documents/444/download/",
            "original_file_name": "leading-slash.pdf"
        })]];
        let mut downloads = HashMap::new();
        downloads.insert(
            444,
            (
                b"%PDF-1.4\n%leading-slash".to_vec(),
                "application/pdf".to_string(),
            ),
        );

        let (base_url, server_task) = start_mock_paperless(pages, downloads).await;

        let report = import_from_api(
            &connection,
            ImportOptions {
                base_url,
                api_token: "paperless-token".to_string(),
                since: None,
                page_size: None,
                dry_run: false,
            },
            None,
        )
        .await
        .expect("import should succeed");

        server_task.abort();

        assert_eq!(report.summary.imported, 1);
        assert_eq!(report.summary.failed, 0);
    }

    #[tokio::test]
    async fn follows_cross_host_next_redirect_and_keeps_auth_header() {
        let connection = new_connection().await;

        let second_pages = vec![vec![json!({
            "id": 555,
            "title": "Redirected Page",
            "original_file_name": "redirected.pdf"
        })]];
        let mut second_downloads = HashMap::new();
        second_downloads.insert(
            555,
            (
                b"%PDF-1.4\n%redirected".to_vec(),
                "application/pdf".to_string(),
            ),
        );
        let (second_base_url, second_server_task) =
            start_auth_required_paperless(second_pages, second_downloads).await;

        let redirected_page_url = format!("{second_base_url}/api/documents/?page=1");
        let (first_base_url, first_server_task) =
            start_redirecting_front_server(redirected_page_url, second_base_url.clone()).await;

        let report = import_from_api(
            &connection,
            ImportOptions {
                base_url: first_base_url,
                api_token: "paperless-token".to_string(),
                since: None,
                page_size: None,
                dry_run: false,
            },
            None,
        )
        .await
        .expect("import should succeed across redirect");

        first_server_task.abort();
        second_server_task.abort();

        assert_eq!(report.summary.scanned, 1);
        assert_eq!(report.summary.imported, 1);
        assert_eq!(report.summary.failed, 0);
    }

    #[test]
    fn parse_datetime_accepts_rfc3339_and_date_only() {
        let with_offset = parse_datetime("2026-03-01T12:40:11+01:00").expect("should parse");
        assert_eq!(with_offset.to_rfc3339(), "2026-03-01T11:40:11+00:00");

        let date_only = parse_datetime("2026-03-01").expect("should parse");
        assert_eq!(date_only.to_rfc3339(), "2026-03-01T00:00:00+00:00");
    }
}
