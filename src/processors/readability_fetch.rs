use dom_smoothie::Readability;
use sea_orm::DatabaseConnection;
use serde::Serialize;

use crate::{
    entity::{
        hyperlink,
        hyperlink_artifact::{self as hyperlink_artifact_entity, HyperlinkArtifactKind},
    },
    model::{
        hyperlink_artifact as hyperlink_artifact_model,
        hyperlink_search_doc as hyperlink_search_doc_model,
    },
    processors::processor::{ProcessingError, Processor},
};

const READABLE_TEXT_CONTENT_TYPE: &str = "text/markdown; charset=utf-8";
const READABLE_META_CONTENT_TYPE: &str = "application/json";
const READABLE_ERROR_CONTENT_TYPE: &str = "application/json";

pub struct ReadabilityFetcher {
    job_id: i32,
    pdf_extractor: Box<dyn PdfTextExtractor>,
}

pub struct ReadabilityFetchOutput {
    pub text_artifact_id: Option<i32>,
    pub meta_artifact_id: Option<i32>,
    pub error_artifact_id: Option<i32>,
}

enum ReadabilitySource {
    Html(hyperlink_artifact_entity::Model),
    Pdf(hyperlink_artifact_entity::Model),
}

trait PdfTextExtractor: Send + Sync {
    fn name(&self) -> &'static str;
    fn extract(&self, payload: &[u8]) -> Result<PdfExtraction, String>;
}

struct PdfExtraction {
    markdown: String,
    page_count: Option<usize>,
}

struct RustPdfExtractor;

impl PdfTextExtractor for RustPdfExtractor {
    fn name(&self) -> &'static str {
        "pdf_extract"
    }

    fn extract(&self, payload: &[u8]) -> Result<PdfExtraction, String> {
        let text = pdf_extract::extract_text_from_mem(payload)
            .map_err(|error| format!("pdf extraction failed: {error}"))?;
        let page_count = estimate_pdf_page_count(&text);
        let markdown = normalize_pdf_markdown(&text);
        if markdown.trim().is_empty() {
            return Err("pdf extraction produced empty text".to_string());
        }
        Ok(PdfExtraction {
            markdown,
            page_count,
        })
    }
}

impl ReadabilityFetcher {
    pub fn new(job_id: i32) -> Self {
        Self::with_pdf_extractor(job_id, Box::new(RustPdfExtractor))
    }

    fn with_pdf_extractor(job_id: i32, pdf_extractor: Box<dyn PdfTextExtractor>) -> Self {
        Self {
            job_id,
            pdf_extractor,
        }
    }
}

impl Processor for ReadabilityFetcher {
    type Output = ReadabilityFetchOutput;

    async fn process<'a>(
        &'a mut self,
        hyperlink: &'a mut hyperlink::ActiveModel,
        connection: &'a DatabaseConnection,
    ) -> Result<Self::Output, ProcessingError> {
        let hyperlink_id = *hyperlink.id.as_ref();
        let source_url = hyperlink.url.as_ref().to_string();

        let snapshot = hyperlink_artifact_model::latest_for_hyperlink_kind(
            connection,
            hyperlink_id,
            HyperlinkArtifactKind::SnapshotWarc,
        )
        .await
        .map_err(ProcessingError::DB)?;

        let source = if let Some(snapshot) = snapshot {
            ReadabilitySource::Html(snapshot)
        } else {
            let pdf_source = hyperlink_artifact_model::latest_for_hyperlink_kind(
                connection,
                hyperlink_id,
                HyperlinkArtifactKind::PdfSource,
            )
            .await
            .map_err(ProcessingError::DB)?;

            let Some(pdf_source) = pdf_source else {
                let error_artifact = persist_readability_error(
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
                    "readability processing requires snapshot_warc or pdf_source artifacts (error_artifact_id={})",
                    error_artifact.id
                )));
            };

            ReadabilitySource::Pdf(pdf_source)
        };

        let extraction = match source {
            ReadabilitySource::Html(snapshot) => {
                let snapshot_payload = hyperlink_artifact_model::load_payload(&snapshot)
                    .await
                    .map_err(ProcessingError::DB)?;
                let html = match extract_html_from_warc(&snapshot_payload) {
                    Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
                    Err(error) => {
                        let error_artifact = persist_readability_error(
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

                extract_from_html(&html)
            }
            ReadabilitySource::Pdf(pdf_source) => {
                let pdf_payload = hyperlink_artifact_model::load_payload(&pdf_source)
                    .await
                    .map_err(ProcessingError::DB)?;
                extract_from_pdf(
                    self.pdf_extractor.as_ref(),
                    hyperlink.title.as_ref(),
                    hyperlink.url.as_ref(),
                    &pdf_payload,
                )
            }
        };

        let (text_payload, meta_payload) = match extraction {
            Ok(payloads) => payloads,
            Err((stage, message)) => {
                let error_artifact = persist_readability_error(
                    connection,
                    hyperlink_id,
                    self.job_id,
                    &source_url,
                    &stage,
                    &message,
                )
                .await
                .map_err(ProcessingError::DB)?;

                // PDF extraction failures are treated as a successful degraded result.
                if matches!(stage.as_str(), "pdf_extract") {
                    return Ok(ReadabilityFetchOutput {
                        text_artifact_id: None,
                        meta_artifact_id: None,
                        error_artifact_id: Some(error_artifact.id),
                    });
                }

                return Err(ProcessingError::FetchError(format!(
                    "{message} (error_artifact_id={})",
                    error_artifact.id
                )));
            }
        };

        let text_artifact = hyperlink_artifact_model::insert(
            connection,
            hyperlink_id,
            Some(self.job_id),
            HyperlinkArtifactKind::ReadableText,
            text_payload.clone(),
            READABLE_TEXT_CONTENT_TYPE,
        )
        .await
        .map_err(ProcessingError::DB)?;

        let readable_text = String::from_utf8_lossy(&text_payload).to_string();
        if let Err(error) = hyperlink_search_doc_model::upsert_readable_text(
            connection,
            hyperlink_id,
            &readable_text,
        )
        .await
        {
            if !hyperlink_search_doc_model::is_search_doc_missing_error(&error) {
                return Err(ProcessingError::DB(error));
            }
            tracing::debug!(
                hyperlink_id,
                "skipping search doc update because hyperlink_search_doc is unavailable"
            );
        }

        let meta_artifact = hyperlink_artifact_model::insert(
            connection,
            hyperlink_id,
            Some(self.job_id),
            HyperlinkArtifactKind::ReadableMeta,
            meta_payload,
            READABLE_META_CONTENT_TYPE,
        )
        .await
        .map_err(ProcessingError::DB)?;

        Ok(ReadabilityFetchOutput {
            text_artifact_id: Some(text_artifact.id),
            meta_artifact_id: Some(meta_artifact.id),
            error_artifact_id: None,
        })
    }
}

async fn persist_readability_error(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    job_id: i32,
    source_url: &str,
    stage: &str,
    error: &str,
) -> Result<hyperlink_artifact_entity::Model, sea_orm::DbErr> {
    let payload = serde_json::to_vec_pretty(&ReadableErrorArtifact {
        source_url: source_url.to_string(),
        stage: stage.to_string(),
        error: error.to_string(),
        failed_at: now_utc().to_string(),
    })
    .unwrap_or_else(|encode_error| {
        format!("{{\"error\":\"failed to encode readable_error artifact: {encode_error}\"}}")
            .into_bytes()
    });

    hyperlink_artifact_model::insert(
        connection,
        hyperlink_id,
        Some(job_id),
        HyperlinkArtifactKind::ReadableError,
        payload,
        READABLE_ERROR_CONTENT_TYPE,
    )
    .await
}

fn extract_from_html(html: &str) -> Result<(Vec<u8>, Vec<u8>), (String, String)> {
    let mut readability = Readability::new(html, None, None).map_err(|error| {
        (
            "readability_init".to_string(),
            format!("failed to initialize dom_smoothie: {error}"),
        )
    })?;
    let article = readability.parse().map_err(|error| {
        (
            "readability_parse".to_string(),
            format!("dom_smoothie parse failed: {error}"),
        )
    })?;

    let content_html = article.content.to_string();
    let markdown = convert_html_to_markdown(&content_html);
    let text_payload = markdown.into_bytes();
    let meta_payload = serde_json::to_vec_pretty(&ReadableMetadataArtifact {
        source_format: "html".to_string(),
        title: article.title,
        byline: article.byline,
        excerpt: article.excerpt,
        site_name: article.site_name,
        dir: article.dir,
        lang: article.lang,
        published_time: article.published_time,
        modified_time: article.modified_time,
        image: article.image,
        favicon: article.favicon,
        url: article.url,
        length: article.length,
        content_html,
        pdf_page_count: None,
        pdf_extractor: None,
    })
    .map_err(|error| {
        (
            "metadata_encode".to_string(),
            format!("failed to encode readability metadata: {error}"),
        )
    })?;

    Ok((text_payload, meta_payload))
}

fn extract_from_pdf(
    extractor: &dyn PdfTextExtractor,
    hyperlink_title: &str,
    source_url: &str,
    payload: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), (String, String)> {
    let extraction = extractor
        .extract(payload)
        .map_err(|error| ("pdf_extract".to_string(), error))?;

    let text_payload = extraction.markdown.clone().into_bytes();
    let meta_payload = serde_json::to_vec_pretty(&ReadableMetadataArtifact {
        source_format: "pdf".to_string(),
        title: hyperlink_title.to_string(),
        byline: None,
        excerpt: None,
        site_name: None,
        dir: None,
        lang: None,
        published_time: None,
        modified_time: None,
        image: None,
        favicon: None,
        url: Some(source_url.to_string()),
        length: extraction.markdown.chars().count(),
        content_html: String::new(),
        pdf_page_count: extraction.page_count,
        pdf_extractor: Some(extractor.name().to_string()),
    })
    .map_err(|error| {
        (
            "metadata_encode".to_string(),
            format!("failed to encode readability metadata: {error}"),
        )
    })?;

    Ok((text_payload, meta_payload))
}

pub(crate) fn extract_html_from_warc(payload: &[u8]) -> Result<Vec<u8>, String> {
    let mut cursor = 0usize;

    while cursor < payload.len() {
        let Some(record_start) = find_subslice(&payload[cursor..], b"WARC/1.0\r\n") else {
            break;
        };
        cursor += record_start;

        let Some(headers_end) = find_subslice(&payload[cursor..], b"\r\n\r\n") else {
            return Err("invalid WARC payload: missing record header terminator".to_string());
        };
        let header_bytes = &payload[cursor..cursor + headers_end];
        let headers = parse_warc_headers(header_bytes)?;

        let content_length = headers
            .get("content-length")
            .ok_or_else(|| "invalid WARC payload: record missing Content-Length".to_string())?
            .parse::<usize>()
            .map_err(|error| format!("invalid WARC payload: bad Content-Length: {error}"))?;

        let record_payload_start = cursor + headers_end + 4;
        let Some(record_payload_end) = record_payload_start.checked_add(content_length) else {
            return Err("invalid WARC payload: content length overflow".to_string());
        };
        if record_payload_end > payload.len() {
            return Err("invalid WARC payload: truncated record body".to_string());
        }

        let warc_type = headers.get("warc-type").map(String::as_str).unwrap_or("");
        let content_type = headers
            .get("content-type")
            .map(String::as_str)
            .unwrap_or("");
        if warc_type.eq_ignore_ascii_case("response") && is_html_content_type(content_type) {
            return Ok(payload[record_payload_start..record_payload_end].to_vec());
        }

        cursor = record_payload_end;
        if payload
            .get(cursor..cursor + 4)
            .is_some_and(|bytes| bytes == b"\r\n\r\n")
        {
            cursor += 4;
        }
    }

    Err("no HTML response record found in WARC payload".to_string())
}

fn parse_warc_headers(
    header_bytes: &[u8],
) -> Result<std::collections::HashMap<String, String>, String> {
    let headers_text = std::str::from_utf8(header_bytes)
        .map_err(|error| format!("invalid WARC headers: {error}"))?;
    let mut lines = headers_text.split("\r\n");

    let Some(version) = lines.next() else {
        return Err("invalid WARC payload: missing version line".to_string());
    };
    if !version.starts_with("WARC/") {
        return Err("invalid WARC payload: record missing WARC version".to_string());
    }

    let mut headers = std::collections::HashMap::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(format!("invalid WARC header line: {line}"));
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }

    Ok(headers)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn is_html_content_type(content_type: &str) -> bool {
    let lower = content_type.to_ascii_lowercase();
    lower.contains("text/html") || lower.contains("application/xhtml+xml")
}

fn convert_html_to_markdown(content_html: &str) -> String {
    html2md::parse_html(content_html)
}

fn normalize_pdf_markdown(text: &str) -> String {
    let with_page_breaks = text.replace('\u{000C}', "\n\n---\n\n");
    let normalized_lines = with_page_breaks
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    normalized_lines.trim().to_string()
}

fn estimate_pdf_page_count(text: &str) -> Option<usize> {
    if text.trim().is_empty() {
        return None;
    }
    let separators = text.chars().filter(|ch| *ch == '\u{000C}').count();
    Some(separators + 1)
}

fn now_utc() -> sea_orm::entity::prelude::DateTime {
    sea_orm::entity::prelude::DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}

#[derive(Serialize)]
struct ReadableMetadataArtifact {
    source_format: String,
    title: String,
    byline: Option<String>,
    excerpt: Option<String>,
    site_name: Option<String>,
    dir: Option<String>,
    lang: Option<String>,
    published_time: Option<String>,
    modified_time: Option<String>,
    image: Option<String>,
    favicon: Option<String>,
    url: Option<String>,
    length: usize,
    content_html: String,
    pdf_page_count: Option<usize>,
    pdf_extractor: Option<String>,
}

#[derive(Serialize)]
struct ReadableErrorArtifact {
    source_url: String,
    stage: String,
    error: String,
    failed_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_html_response_record_from_warc() {
        let warc = concat!(
            "WARC/1.0\r\n",
            "WARC-Type: response\r\n",
            "WARC-Target-URI: https://example.com/\r\n",
            "Content-Type: text/html; charset=utf-8\r\n",
            "Content-Length: 16\r\n",
            "\r\n",
            "<html>ok</html>\n",
            "\r\n\r\n",
        );
        let html = extract_html_from_warc(warc.as_bytes()).expect("html record should parse");
        assert_eq!(String::from_utf8_lossy(&html), "<html>ok</html>\n");
    }

    #[test]
    fn converts_readability_html_to_markdown() {
        let html = "<h1>Title</h1><p>Hello <a href=\"https://example.com\">world</a></p>";
        let markdown = convert_html_to_markdown(html);
        assert!(markdown.contains("Title"));
        assert!(markdown.contains("[world](https://example.com)"));
    }

    #[test]
    fn normalizes_pdf_markdown_page_breaks() {
        let normalized = normalize_pdf_markdown("Page one\u{000C}Page two");
        assert_eq!(normalized, "Page one\n\n---\n\nPage two");
    }

    #[test]
    fn estimates_pdf_page_count_from_form_feed() {
        assert_eq!(
            estimate_pdf_page_count("first\u{000C}second\u{000C}third"),
            Some(3)
        );
        assert_eq!(estimate_pdf_page_count("   "), None);
    }

    struct FakePdfExtractor;

    impl PdfTextExtractor for FakePdfExtractor {
        fn name(&self) -> &'static str {
            "fake"
        }

        fn extract(&self, _payload: &[u8]) -> Result<PdfExtraction, String> {
            Ok(PdfExtraction {
                markdown: "sample text".to_string(),
                page_count: Some(2),
            })
        }
    }

    #[test]
    fn extracts_pdf_text_and_metadata() {
        let (text_payload, meta_payload) = extract_from_pdf(
            &FakePdfExtractor,
            "Doc",
            "https://example.com/report.pdf",
            b"%PDF",
        )
        .expect("pdf extraction should succeed");

        assert_eq!(
            String::from_utf8(text_payload).expect("text payload should decode"),
            "sample text"
        );
        let meta: serde_json::Value =
            serde_json::from_slice(&meta_payload).expect("meta should decode");
        assert_eq!(meta["source_format"], "pdf");
        assert_eq!(meta["title"], "Doc");
        assert_eq!(meta["pdf_page_count"], 2);
        assert_eq!(meta["pdf_extractor"], "fake");
    }
}
