use async_trait::async_trait;
use dom_smoothie::Readability;
use reqwest::{
    StatusCode,
    header::{HeaderName, HeaderValue},
};
use sea_orm::{ActiveValue::Set, DatabaseConnection};
use serde::Serialize;
use std::{
    any::Any,
    panic::{AssertUnwindSafe, catch_unwind},
    time::{Duration, Instant},
};

use crate::{
    app::models::{
        hyperlink_artifact as hyperlink_artifact_model,
        hyperlink_search_doc as hyperlink_search_doc_model, hyperlink_title,
        llm_discovery::{
            ChatApiKind, build_chat_request_body, chat_endpoint_candidates,
            format_reqwest_transport_error,
        },
        llm_interaction as llm_interaction_model,
        llm_settings::{self, LlmBackendKind, LlmSettings},
    },
    entity::{
        hyperlink,
        hyperlink_artifact::{self as hyperlink_artifact_entity, HyperlinkArtifactKind},
    },
    integrations::mathpix::{self, MathpixConfig, MathpixMode},
    processors::processor::{ProcessingError, Processor},
};

const READABLE_TEXT_CONTENT_TYPE: &str = "text/markdown; charset=utf-8";
const READABLE_HTML_CONTENT_TYPE: &str = "text/html; charset=utf-8";
const READABLE_META_CONTENT_TYPE: &str = "application/json";
const READABLE_ERROR_CONTENT_TYPE: &str = "application/json";
const MATHPIX_BASE_URL: &str = "https://api.mathpix.com";
const MATHPIX_SUBMIT_PDF_PATH: &str = "/v3/pdf";
const PDF_LLM_INTERACTION_KIND: &str = "pdf_enrichment";
const PDF_LLM_TIMEOUT: Duration = Duration::from_secs(20);
const PDF_LLM_MARKDOWN_CHAR_LIMIT: usize = 16_000;
const PDF_SUMMARY_CHAR_LIMIT: usize = 320;

pub struct ReadabilityFetcher {
    job_id: i32,
    pdf_extractor: Box<dyn PdfTextExtractor>,
    mathpix_pdf_extractor: Option<Box<dyn PdfTextExtractor>>,
}

pub struct ReadabilityFetchOutput {
    pub text_artifact_id: Option<i32>,
    pub html_artifact_id: Option<i32>,
    pub meta_artifact_id: Option<i32>,
    pub error_artifact_id: Option<i32>,
}

enum ReadabilitySource {
    Html(hyperlink_artifact_entity::Model),
    Pdf(hyperlink_artifact_entity::Model),
}

#[async_trait]
trait PdfTextExtractor: Send + Sync {
    fn name(&self) -> &'static str;
    async fn extract(&self, payload: &[u8]) -> Result<PdfExtraction, String>;
}

#[derive(Clone, Debug)]
struct PdfExtraction {
    markdown: String,
    rendered_html: Option<String>,
    page_count: Option<usize>,
    title: Option<String>,
}

#[derive(Debug)]
struct ReadabilityArtifactsPayload {
    text_payload: Vec<u8>,
    html_payload: Option<Vec<u8>>,
    meta_payload: Vec<u8>,
    readability_title: Option<String>,
}

struct RustPdfExtractor;

struct MathpixPdfExtractor {
    client: reqwest::Client,
    app_id: String,
    app_key: String,
    poll_interval: std::time::Duration,
    poll_timeout: std::time::Duration,
}

#[derive(Clone, Debug)]
struct MathpixSubmitResponse {
    pdf_id: String,
}

#[derive(Clone, Debug)]
struct MathpixPollResult {
    page_count: Option<usize>,
}

#[async_trait]
impl PdfTextExtractor for RustPdfExtractor {
    fn name(&self) -> &'static str {
        "pdf_extract"
    }

    async fn extract(&self, payload: &[u8]) -> Result<PdfExtraction, String> {
        let text = pdf_extract::extract_text_from_mem(payload)
            .map_err(|error| format!("pdf extraction failed: {error}"))?;
        let page_count = estimate_pdf_page_count(&text);
        let markdown = normalize_pdf_markdown(&text);
        if markdown.trim().is_empty() {
            return Err("pdf extraction produced empty text".to_string());
        }
        Ok(PdfExtraction {
            title: extract_pdf_metadata_title(payload)
                .or_else(|| infer_pdf_title_from_markdown(&markdown)),
            markdown,
            rendered_html: None,
            page_count,
        })
    }
}

impl MathpixPdfExtractor {
    fn new(config: MathpixConfig) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .map_err(|error| format!("failed to build mathpix client: {error}"))?;
        Ok(Self {
            client,
            app_id: config.app_id,
            app_key: config.app_key,
            poll_interval: config.poll_interval,
            poll_timeout: config.poll_timeout,
        })
    }

    fn set_auth_headers(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, String> {
        let app_id = HeaderValue::from_str(&self.app_id)
            .map_err(|error| format!("invalid mathpix app_id header value: {error}"))?;
        let app_key = HeaderValue::from_str(&self.app_key)
            .map_err(|error| format!("invalid mathpix app_key header value: {error}"))?;
        Ok(request.header("app_id", app_id).header("app_key", app_key))
    }

    async fn submit_pdf(&self, payload: &[u8]) -> Result<MathpixSubmitResponse, String> {
        let file_part = reqwest::multipart::Part::bytes(payload.to_vec())
            .file_name("document.pdf")
            .mime_str("application/pdf")
            .map_err(|error| format!("failed to build mathpix upload payload: {error}"))?;
        let options_part = reqwest::multipart::Part::text(
            r#"{"conversion_formats":{"md":true,"html":true},"math_inline_delimiters":["$","$"]}"#,
        )
        .mime_str("application/json")
        .map_err(|error| format!("failed to build mathpix options payload: {error}"))?;
        let form = reqwest::multipart::Form::new()
            .part("file", file_part)
            .part("options_json", options_part);

        let request = self
            .client
            .post(format!("{MATHPIX_BASE_URL}{MATHPIX_SUBMIT_PDF_PATH}"))
            .multipart(form);
        let request = self.set_auth_headers(request)?;

        let response = request
            .send()
            .await
            .map_err(|error| format!("failed to submit pdf to mathpix: {error}"))?;
        parse_mathpix_submit_response(response).await
    }

    async fn poll_until_complete(&self, pdf_id: &str) -> Result<MathpixPollResult, String> {
        let status_url = format!("{MATHPIX_BASE_URL}/v3/pdf/{pdf_id}");
        let deadline = Instant::now() + self.poll_timeout;

        loop {
            if Instant::now() > deadline {
                return Err(format!(
                    "mathpix processing timed out after {}s",
                    self.poll_timeout.as_secs()
                ));
            }

            let request = self.client.get(status_url.clone());
            let request = self.set_auth_headers(request)?;
            let response = request.send().await.map_err(|error| {
                format!("failed to poll mathpix status for pdf_id {pdf_id}: {error}")
            })?;
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(format!(
                    "mathpix status poll failed for pdf_id {pdf_id}: status {} ({})",
                    status,
                    summarize_api_error(&body)
                ));
            }

            let body_text = response
                .text()
                .await
                .map_err(|error| format!("failed to decode mathpix poll response: {error}"))?;
            let body: serde_json::Value = serde_json::from_str(&body_text)
                .map_err(|error| format!("failed to parse mathpix poll response: {error}"))?;
            let status = body
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            if is_mathpix_completed_status(&status) {
                return Ok(MathpixPollResult {
                    page_count: infer_mathpix_page_count(&body),
                });
            }
            if is_mathpix_failed_status(&status) {
                let reason = body
                    .get("error")
                    .and_then(|value| value.as_str())
                    .or_else(|| body.get("error_info").and_then(|value| value.as_str()))
                    .or_else(|| body.get("message").and_then(|value| value.as_str()))
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or("unknown error");
                return Err(format!(
                    "mathpix reported failure for pdf_id {pdf_id} (status={status}): {reason}"
                ));
            }

            tokio::time::sleep(self.poll_interval).await;
        }
    }

    async fn fetch_markdown(&self, pdf_id: &str) -> Result<String, String> {
        // Mathpix may expose either `.mmd` or `.md` depending on account/output settings.
        let mut failures = Vec::new();
        for suffix in [".mmd", ".md"] {
            let url = format!("{MATHPIX_BASE_URL}/v3/pdf/{pdf_id}{suffix}");
            let request = self.client.get(url);
            let request = self.set_auth_headers(request)?;
            let response = request
                .send()
                .await
                .map_err(|error| format!("failed to fetch mathpix markdown: {error}"))?;

            if response.status() == StatusCode::NOT_FOUND {
                failures.push(format!("{suffix}: not found"));
                continue;
            }

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                failures.push(format!(
                    "{suffix}: status {} ({})",
                    status,
                    summarize_api_error(&body)
                ));
                continue;
            }

            let markdown = response
                .text()
                .await
                .map_err(|error| format!("failed to decode mathpix markdown payload: {error}"))?;
            let normalized = normalize_pdf_markdown(&markdown);
            if normalized.trim().is_empty() {
                failures.push(format!("{suffix}: markdown output was empty"));
                continue;
            }

            return Ok(normalized);
        }

        Err(format!(
            "mathpix markdown fetch failed for pdf_id {pdf_id}: {}",
            failures.join("; ")
        ))
    }

    async fn fetch_html(&self, pdf_id: &str) -> Result<Option<String>, String> {
        let url = format!("{MATHPIX_BASE_URL}/v3/pdf/{pdf_id}.html");
        let request = self.client.get(url);
        let request = self.set_auth_headers(request)?;
        let response = request
            .send()
            .await
            .map_err(|error| format!("failed to fetch mathpix html: {error}"))?;

        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            tracing::warn!(
                pdf_id,
                status = %status,
                error = %summarize_api_error(&body),
                "mathpix html conversion was unavailable; continuing with markdown only"
            );
            return Ok(None);
        }

        let html = response
            .text()
            .await
            .map_err(|error| format!("failed to decode mathpix html payload: {error}"))?;
        let trimmed = html.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }

        Ok(Some(trimmed.to_string()))
    }
}

#[async_trait]
impl PdfTextExtractor for MathpixPdfExtractor {
    fn name(&self) -> &'static str {
        "mathpix"
    }

    async fn extract(&self, payload: &[u8]) -> Result<PdfExtraction, String> {
        if payload.is_empty() {
            return Err("mathpix payload was empty".to_string());
        }
        let submit = self.submit_pdf(payload).await?;
        let poll = self.poll_until_complete(&submit.pdf_id).await?;
        let markdown = self.fetch_markdown(&submit.pdf_id).await?;
        let rendered_html = self.fetch_html(&submit.pdf_id).await?;
        Ok(PdfExtraction {
            title: extract_pdf_metadata_title(payload)
                .or_else(|| infer_pdf_title_from_markdown(&markdown)),
            markdown,
            rendered_html,
            page_count: poll.page_count,
        })
    }
}

impl ReadabilityFetcher {
    pub fn new(job_id: i32) -> Self {
        let mathpix_pdf_extractor = match mathpix::load_mode_from_env() {
            MathpixMode::Enabled(config) => match MathpixPdfExtractor::new(config) {
                Ok(extractor) => Some(Box::new(extractor) as Box<dyn PdfTextExtractor>),
                Err(error) => {
                    tracing::warn!(
                        job_id,
                        error = %error,
                        "mathpix pdf extractor is enabled but failed to initialize; falling back to pdf_extract"
                    );
                    None
                }
            },
            mode => {
                if mode.disabled_missing_app_id() {
                    tracing::warn!(
                        job_id,
                        "MATHPIX_API_TOKEN is set but MATHPIX_APP_ID is missing; falling back to pdf_extract"
                    );
                }
                None
            }
        };

        Self::with_pdf_extractors(job_id, Box::new(RustPdfExtractor), mathpix_pdf_extractor)
    }

    fn with_pdf_extractors(
        job_id: i32,
        pdf_extractor: Box<dyn PdfTextExtractor>,
        mathpix_pdf_extractor: Option<Box<dyn PdfTextExtractor>>,
    ) -> Self {
        Self {
            job_id,
            pdf_extractor,
            mathpix_pdf_extractor,
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
                let snapshot_payload = hyperlink_artifact_model::load_processing_payload(&snapshot)
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

                extract_from_html(&html).map(|(text_payload, meta_payload)| {
                    ReadabilityArtifactsPayload {
                        text_payload,
                        html_payload: None,
                        meta_payload,
                        readability_title: None,
                    }
                })
            }
            ReadabilitySource::Pdf(pdf_source) => {
                let pdf_payload = hyperlink_artifact_model::load_payload(&pdf_source)
                    .await
                    .map_err(ProcessingError::DB)?;
                extract_from_pdf_with_fallback(
                    self.mathpix_pdf_extractor.as_deref(),
                    self.pdf_extractor.as_ref(),
                    hyperlink.title.as_ref(),
                    hyperlink.url.as_ref(),
                    &pdf_payload,
                )
                .await
            }
        };

        let ReadabilityArtifactsPayload {
            text_payload,
            html_payload,
            meta_payload,
            readability_title,
        } = match extraction {
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
                        html_artifact_id: None,
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

        let readable_text = String::from_utf8_lossy(&text_payload).to_string();

        if let Some(readability_title) = readability_title.as_deref() {
            let cleaned_title = hyperlink_title::strip_site_affixes(
                readability_title,
                hyperlink.url.as_ref(),
                hyperlink.raw_url.as_ref(),
            );
            if !cleaned_title.is_empty() && cleaned_title != hyperlink.title.as_ref().as_str() {
                hyperlink.title = Set(cleaned_title);
            }
        }

        if should_attempt_pdf_llm_enrichment(hyperlink) {
            match enrich_uploaded_pdf_with_llm(
                connection,
                hyperlink_id,
                self.job_id,
                hyperlink,
                &readable_text,
                readability_title.as_deref(),
            )
            .await
            {
                Ok(Some(enrichment)) => {
                    if let Some(title) = enrichment.title {
                        hyperlink.title = Set(title);
                    }
                    if let Some(summary) = enrichment.summary {
                        hyperlink.summary = Set(Some(summary));
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        hyperlink_id,
                        job_id = self.job_id,
                        error = %error,
                        "uploaded pdf llm enrichment failed; continuing without ai metadata"
                    );
                }
            }
        }

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

        let html_artifact = if let Some(html_payload) = html_payload {
            Some(
                hyperlink_artifact_model::insert(
                    connection,
                    hyperlink_id,
                    Some(self.job_id),
                    HyperlinkArtifactKind::ReadableHtml,
                    html_payload,
                    READABLE_HTML_CONTENT_TYPE,
                )
                .await
                .map_err(ProcessingError::DB)?,
            )
        } else {
            None
        };

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
            html_artifact_id: html_artifact.map(|artifact| artifact.id),
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
    if looks_like_frameset_document(html) {
        return Err((
            "readability_parse".to_string(),
            "frameset documents are not supported by readability extraction".to_string(),
        ));
    }

    let mut readability = Readability::new(html, None, None).map_err(|error| {
        (
            "readability_init".to_string(),
            format!("failed to initialize dom_smoothie: {error}"),
        )
    })?;
    let article = catch_unwind(AssertUnwindSafe(|| readability.parse()))
        .map_err(|panic_payload| {
            (
                "readability_parse".to_string(),
                format!(
                    "dom_smoothie panicked while parsing HTML: {}",
                    panic_message(&panic_payload)
                ),
            )
        })?
        .map_err(|error| {
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

async fn extract_from_pdf_with_fallback(
    primary_extractor: Option<&dyn PdfTextExtractor>,
    fallback_extractor: &dyn PdfTextExtractor,
    hyperlink_title: &str,
    source_url: &str,
    payload: &[u8],
) -> Result<ReadabilityArtifactsPayload, (String, String)> {
    let Some(primary_extractor) = primary_extractor else {
        return extract_from_pdf(fallback_extractor, hyperlink_title, source_url, payload).await;
    };

    match extract_from_pdf(primary_extractor, hyperlink_title, source_url, payload).await {
        Ok(extraction) => Ok(extraction),
        Err((_stage, primary_error)) => {
            tracing::warn!(
                extractor = primary_extractor.name(),
                error = %primary_error,
                fallback = fallback_extractor.name(),
                "primary pdf extraction failed; attempting fallback extractor"
            );
            match extract_from_pdf(fallback_extractor, hyperlink_title, source_url, payload).await {
                Ok(extraction) => Ok(extraction),
                Err((stage, fallback_error)) => Err((
                    stage,
                    format!(
                        "{} failed: {primary_error}; fallback {} failed: {fallback_error}",
                        primary_extractor.name(),
                        fallback_extractor.name()
                    ),
                )),
            }
        }
    }
}

async fn extract_from_pdf(
    extractor: &dyn PdfTextExtractor,
    hyperlink_title: &str,
    source_url: &str,
    payload: &[u8],
) -> Result<ReadabilityArtifactsPayload, (String, String)> {
    let extraction = extractor
        .extract(payload)
        .await
        .map_err(|error| ("pdf_extract".to_string(), error))?;

    let extracted_title = extraction
        .title
        .as_deref()
        .and_then(normalize_readability_title);
    let metadata_title = extracted_title
        .as_deref()
        .unwrap_or(hyperlink_title)
        .to_string();

    let text_payload = extraction.markdown.clone().into_bytes();
    let html_payload = extraction.rendered_html.map(|html| html.into_bytes());
    let meta_payload = serde_json::to_vec_pretty(&ReadableMetadataArtifact {
        source_format: "pdf".to_string(),
        title: metadata_title,
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

    Ok(ReadabilityArtifactsPayload {
        text_payload,
        html_payload,
        meta_payload,
        readability_title: extracted_title,
    })
}

#[derive(Clone, Debug, Default)]
struct PdfLlmEnrichment {
    title: Option<String>,
    summary: Option<String>,
}

impl PdfLlmEnrichment {
    fn is_empty(&self) -> bool {
        self.title.is_none() && self.summary.is_none()
    }
}

async fn enrich_uploaded_pdf_with_llm(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    job_id: i32,
    hyperlink: &hyperlink::ActiveModel,
    readable_text: &str,
    extracted_title: Option<&str>,
) -> Result<Option<PdfLlmEnrichment>, String> {
    let settings = llm_settings::load(connection)
        .await
        .map_err(|error| format!("failed to load llm settings: {error}"))?;
    if !llm_settings_are_configured(&settings) {
        return Ok(None);
    }

    let endpoints = chat_endpoint_candidates(&settings.base_url, settings.backend_kind)
        .map_err(|error| format!("invalid llm settings: {error}"))?;
    if endpoints.is_empty() {
        return Ok(None);
    }

    let client = reqwest::Client::builder()
        .timeout(PDF_LLM_TIMEOUT)
        .build()
        .map_err(|error| format!("failed to build llm client: {error}"))?;
    let auth_header = build_llm_auth_header(&settings)?;
    let provider = settings.provider.as_storage().to_string();

    let user_prompt = serde_json::to_string_pretty(&serde_json::json!({
        "source_url": hyperlink.url.as_ref(),
        "submitted_url": hyperlink.raw_url.as_ref(),
        "current_title": hyperlink.title.as_ref(),
        "extracted_title": extracted_title,
        "markdown_excerpt": truncate_pdf_markdown_for_llm(readable_text),
    }))
    .unwrap_or_else(|_| readable_text.to_string());
    let system_prompt = concat!(
        "You extract metadata for uploaded PDF documents in a personal hyperlinks app. ",
        "Reply with strict JSON only using the shape {\"title\": string|null, \"summary\": string|null}. ",
        "Title must be the real document title, never a filename, URL, upload path, LaTeX wrapper like \\title{, author list, or abstract heading. ",
        "Summary should be a concise plain-text description suitable for replacing a meaningless upload URL in a list. ",
        "Keep summary to 1-2 sentences and at most 320 characters. Do not include markdown."
    );

    let mut attempt_failures = Vec::new();
    for endpoint in endpoints {
        let endpoint_url = endpoint.url.to_string();
        let body = build_chat_request_body(
            endpoint.api_kind,
            &settings.model,
            system_prompt,
            &user_prompt,
        );
        let request_body = llm_interaction_model::format_request_body(&body);
        let started = Instant::now();
        let mut request = client
            .post(endpoint.url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&body);
        if let Some((header_name, header_value)) = auth_header.as_ref() {
            request = request.header(header_name.clone(), header_value.clone());
        }

        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                let error_message = format_reqwest_transport_error(&error);
                record_pdf_llm_interaction(
                    connection,
                    hyperlink_id,
                    job_id,
                    &provider,
                    &settings.model,
                    &endpoint_url,
                    endpoint.api_kind,
                    &request_body,
                    None,
                    None,
                    Some(error_message.clone()),
                    started.elapsed(),
                )
                .await;
                attempt_failures.push(format!(
                    "{} [{}] -> request failed: {}",
                    endpoint_url,
                    endpoint.api_kind.as_str(),
                    error_message
                ));
                continue;
            }
        };

        let status = response.status();
        let response_body = match response.text().await {
            Ok(body) => body,
            Err(error) => {
                let error_message = format!("failed to read response body: {error}");
                record_pdf_llm_interaction(
                    connection,
                    hyperlink_id,
                    job_id,
                    &provider,
                    &settings.model,
                    &endpoint_url,
                    endpoint.api_kind,
                    &request_body,
                    None,
                    Some(status),
                    Some(error_message.clone()),
                    started.elapsed(),
                )
                .await;
                attempt_failures.push(format!(
                    "{} [{}] -> failed to read response body: {}",
                    endpoint_url,
                    endpoint.api_kind.as_str(),
                    error_message
                ));
                continue;
            }
        };

        if !status.is_success() {
            let error_message = format!("status {status}: {}", summarize_api_error(&response_body));
            record_pdf_llm_interaction(
                connection,
                hyperlink_id,
                job_id,
                &provider,
                &settings.model,
                &endpoint_url,
                endpoint.api_kind,
                &request_body,
                Some(response_body.clone()),
                Some(status),
                Some(error_message.clone()),
                started.elapsed(),
            )
            .await;
            attempt_failures.push(format!(
                "{} [{}] -> {}",
                endpoint_url,
                endpoint.api_kind.as_str(),
                error_message
            ));
            continue;
        }

        let parsed_response = match parse_pdf_llm_chat_response(&response_body) {
            Ok(parsed) => parsed,
            Err(error_message) => {
                record_pdf_llm_interaction(
                    connection,
                    hyperlink_id,
                    job_id,
                    &provider,
                    &settings.model,
                    &endpoint_url,
                    endpoint.api_kind,
                    &request_body,
                    Some(response_body.clone()),
                    Some(status),
                    Some(error_message.clone()),
                    started.elapsed(),
                )
                .await;
                attempt_failures.push(format!(
                    "{} [{}] -> {}",
                    endpoint_url,
                    endpoint.api_kind.as_str(),
                    error_message
                ));
                continue;
            }
        };

        let enrichment = normalize_pdf_llm_enrichment(
            parsed_response.title.as_deref(),
            parsed_response.summary.as_deref(),
            hyperlink.url.as_ref(),
            hyperlink.raw_url.as_ref(),
        );
        if enrichment.is_empty() {
            let error_message =
                "llm response did not contain a usable title or summary".to_string();
            record_pdf_llm_interaction(
                connection,
                hyperlink_id,
                job_id,
                &provider,
                &settings.model,
                &endpoint_url,
                endpoint.api_kind,
                &request_body,
                Some(response_body.clone()),
                Some(status),
                Some(error_message.clone()),
                started.elapsed(),
            )
            .await;
            attempt_failures.push(format!(
                "{} [{}] -> {}",
                endpoint_url,
                endpoint.api_kind.as_str(),
                error_message
            ));
            continue;
        }

        record_pdf_llm_interaction(
            connection,
            hyperlink_id,
            job_id,
            &provider,
            &settings.model,
            &endpoint_url,
            endpoint.api_kind,
            &request_body,
            Some(response_body),
            Some(status),
            None,
            started.elapsed(),
        )
        .await;
        return Ok(Some(enrichment));
    }

    if attempt_failures.is_empty() {
        Ok(None)
    } else {
        Err(format!(
            "all configured llm endpoint candidates failed: {}",
            attempt_failures.join(" | ")
        ))
    }
}

fn llm_settings_are_configured(settings: &LlmSettings) -> bool {
    settings.backend_kind != LlmBackendKind::Unknown
        && !settings.base_url.trim().is_empty()
        && !settings.model.trim().is_empty()
}

fn should_attempt_pdf_llm_enrichment(hyperlink: &hyperlink::ActiveModel) -> bool {
    let is_pdf = matches!(
        hyperlink.source_type.as_ref(),
        &hyperlink::HyperlinkSourceType::Pdf
    );
    let is_upload = is_uploaded_pdf_url(hyperlink.url.as_ref());
    let has_summary = hyperlink
        .summary
        .as_ref()
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());

    is_pdf && is_upload && !has_summary
}

fn is_uploaded_pdf_url(value: &str) -> bool {
    value.trim_start().starts_with("/uploads/")
}

fn truncate_pdf_markdown_for_llm(markdown: &str) -> String {
    let normalized = markdown.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= PDF_LLM_MARKDOWN_CHAR_LIMIT {
        return normalized;
    }

    let mut truncated = normalized
        .chars()
        .take(PDF_LLM_MARKDOWN_CHAR_LIMIT)
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

fn build_llm_auth_header(
    settings: &LlmSettings,
) -> Result<Option<(HeaderName, HeaderValue)>, String> {
    let Some(api_key) = settings.api_key.as_deref() else {
        return Ok(None);
    };

    let header_name = settings
        .auth_header_name
        .clone()
        .unwrap_or_else(|| "Authorization".to_string());
    let header_prefix = settings
        .auth_header_prefix
        .clone()
        .unwrap_or_else(|| "Bearer".to_string());
    let header_value = if header_prefix.trim().is_empty() {
        api_key.to_string()
    } else {
        format!("{} {}", header_prefix.trim(), api_key)
    };

    let header_name = HeaderName::from_bytes(header_name.as_bytes())
        .map_err(|error| format!("invalid llm auth header name: {error}"))?;
    let header_value = HeaderValue::from_str(&header_value)
        .map_err(|error| format!("invalid llm auth header value: {error}"))?;

    Ok(Some((header_name, header_value)))
}

#[derive(Clone, Debug, Default)]
struct ParsedPdfLlmResponse {
    title: Option<String>,
    summary: Option<String>,
}

fn parse_pdf_llm_chat_response(body: &str) -> Result<ParsedPdfLlmResponse, String> {
    let payload: serde_json::Value = serde_json::from_str(body)
        .map_err(|error| format!("llm response was not valid json: {error}"))?;
    let content = extract_llm_chat_content(&payload)
        .ok_or_else(|| "llm response did not include assistant message content".to_string())?;
    let content_json = parse_jsonish_llm_content(&content)?;

    Ok(ParsedPdfLlmResponse {
        title: content_json
            .get("title")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
        summary: content_json
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
    })
}

fn extract_llm_chat_content(payload: &serde_json::Value) -> Option<String> {
    payload
        .pointer("/choices/0/message/content")
        .and_then(extract_llm_content_value)
        .or_else(|| {
            payload
                .pointer("/message/content")
                .and_then(extract_llm_content_value)
        })
        .or_else(|| payload.get("response").and_then(extract_llm_content_value))
}

fn extract_llm_content_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Array(items) => {
            let mut combined = String::new();
            for item in items {
                if let Some(text) = extract_llm_content_value(item) {
                    combined.push_str(&text);
                }
            }
            if combined.trim().is_empty() {
                None
            } else {
                Some(combined)
            }
        }
        serde_json::Value::Object(map) => map
            .get("text")
            .and_then(serde_json::Value::as_str)
            .or_else(|| map.get("content").and_then(serde_json::Value::as_str))
            .map(ToString::to_string),
        _ => None,
    }
}

fn parse_jsonish_llm_content(content: &str) -> Result<serde_json::Value, String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err("assistant message content was empty".to_string());
    }

    let unfenced = strip_markdown_code_fences(trimmed);
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&unfenced) {
        return Ok(parsed);
    }

    let Some(start) = unfenced.find('{') else {
        return Err("assistant message did not contain a json object".to_string());
    };
    let Some(end) = unfenced.rfind('}') else {
        return Err("assistant message did not contain a complete json object".to_string());
    };
    serde_json::from_str::<serde_json::Value>(&unfenced[start..=end])
        .map_err(|error| format!("assistant message json parse failed: {error}"))
}

fn strip_markdown_code_fences(value: &str) -> String {
    let trimmed = value.trim();
    let Some(stripped) = trimmed.strip_prefix("```") else {
        return trimmed.to_string();
    };

    let stripped = stripped
        .strip_prefix("json")
        .or_else(|| stripped.strip_prefix("JSON"))
        .unwrap_or(stripped)
        .trim_start_matches(['\r', '\n']);
    stripped
        .strip_suffix("```")
        .unwrap_or(stripped)
        .trim()
        .to_string()
}

fn normalize_pdf_llm_enrichment(
    title: Option<&str>,
    summary: Option<&str>,
    url: &str,
    raw_url: &str,
) -> PdfLlmEnrichment {
    let title = title
        .and_then(normalize_readability_title)
        .map(|title| hyperlink_title::strip_site_affixes(&title, url, raw_url))
        .filter(|title| !title.trim().is_empty());
    let summary = summary.and_then(normalize_summary_candidate);

    PdfLlmEnrichment { title, summary }
}

fn normalize_summary_candidate(raw: &str) -> Option<String> {
    let mut normalized = raw
        .trim()
        .trim_start_matches("Summary:")
        .trim_start_matches("summary:")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if normalized.is_empty() {
        return None;
    }
    if normalized.starts_with("http://") || normalized.starts_with("https://") {
        return None;
    }

    if normalized.chars().count() > PDF_SUMMARY_CHAR_LIMIT {
        normalized = normalized
            .chars()
            .take(PDF_SUMMARY_CHAR_LIMIT)
            .collect::<String>();
        normalized.push('…');
    }

    Some(normalized)
}

async fn record_pdf_llm_interaction(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    job_id: i32,
    provider: &str,
    model: &str,
    endpoint_url: &str,
    api_kind: ChatApiKind,
    request_body: &str,
    response_body: Option<String>,
    response_status: Option<reqwest::StatusCode>,
    error_message: Option<String>,
    duration: Duration,
) {
    if let Err(error) = llm_interaction_model::record(
        connection,
        llm_interaction_model::NewLlmInteraction {
            kind: PDF_LLM_INTERACTION_KIND.to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            endpoint_url: endpoint_url.to_string(),
            api_kind: api_kind.as_str().to_string(),
            hyperlink_id: Some(hyperlink_id),
            processing_job_id: Some(job_id),
            admin_job_kind: None,
            admin_job_id: None,
            request_body: request_body.to_string(),
            response_body,
            response_status: response_status.map(|status| i32::from(status.as_u16())),
            error_message,
            duration_ms: Some(llm_interaction_model::duration_ms(duration)),
            created_at: None,
        },
    )
    .await
    {
        tracing::warn!(hyperlink_id, job_id, error = %error, "failed to record pdf llm interaction");
    }
}

async fn parse_mathpix_submit_response(
    response: reqwest::Response,
) -> Result<MathpixSubmitResponse, String> {
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "mathpix pdf submit failed: status {} ({})",
            status,
            summarize_api_error(&body)
        ));
    }

    let body_text = response
        .text()
        .await
        .map_err(|error| format!("failed to decode mathpix submit response: {error}"))?;
    let body: serde_json::Value = serde_json::from_str(&body_text)
        .map_err(|error| format!("failed to parse mathpix submit response: {error}"))?;
    let Some(pdf_id) = body.get("pdf_id").and_then(|value| value.as_str()) else {
        return Err("mathpix submit response did not include pdf_id".to_string());
    };
    if pdf_id.trim().is_empty() {
        return Err("mathpix submit response contained empty pdf_id".to_string());
    }

    Ok(MathpixSubmitResponse {
        pdf_id: pdf_id.trim().to_string(),
    })
}

fn summarize_api_error(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "empty response body".to_string();
    }
    let summary = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    let max_len = 320;
    if summary.chars().count() <= max_len {
        summary
    } else {
        format!("{}...", summary.chars().take(max_len).collect::<String>())
    }
}

fn is_mathpix_completed_status(status: &str) -> bool {
    matches!(
        status,
        "completed" | "complete" | "done" | "success" | "succeeded"
    )
}

fn is_mathpix_failed_status(status: &str) -> bool {
    matches!(status, "error" | "failed" | "failure")
}

fn infer_mathpix_page_count(value: &serde_json::Value) -> Option<usize> {
    value
        .get("num_pages")
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            value
                .get("num_pages_total")
                .and_then(serde_json::Value::as_u64)
        })
        .map(|count| count as usize)
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

fn looks_like_frameset_document(html: &str) -> bool {
    html.to_ascii_lowercase().contains("<frameset")
}

fn panic_message(panic_payload: &(dyn Any + Send)) -> String {
    if let Some(message) = panic_payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = panic_payload.downcast_ref::<String>() {
        return message.clone();
    }
    "non-string panic payload".to_string()
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

fn extract_pdf_metadata_title(payload: &[u8]) -> Option<String> {
    let document = pdf_extract::Document::load_mem(payload).ok()?;
    let info = document.trailer.get(b"Info").ok()?;
    let info = match info {
        pdf_extract::Object::Reference(id) => document.get_object(*id).ok()?,
        other => other,
    };
    let info = info.as_dict().ok()?;
    let title = info.get(b"Title").ok()?;
    let decoded = pdf_extract::decode_text_string(title).ok()?;
    normalize_readability_title(&decoded)
}

fn infer_pdf_title_from_markdown(markdown: &str) -> Option<String> {
    let lines = markdown.lines().take(24).collect::<Vec<_>>();
    for prefix in ["\\title{", "\\section*{", "\\section{"] {
        if let Some(title) = extract_wrapped_pdf_title(&lines, prefix) {
            return Some(title);
        }
    }

    lines.into_iter().find_map(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == "---" || looks_like_pdf_wrapper_line(trimmed) {
            return None;
        }

        let candidate = strip_inline_pdf_title_command(trimmed)
            .trim_start_matches('#')
            .trim()
            .trim_matches(|ch: char| {
                matches!(
                    ch,
                    '{' | '}' | '-' | '•' | '*' | '·' | '—' | '–' | ':' | '"' | '\'' | '“' | '”'
                )
            })
            .trim();
        normalize_readability_title(candidate)
    })
}

fn extract_wrapped_pdf_title(lines: &[&str], prefix: &str) -> Option<String> {
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let Some(remainder) = trimmed.strip_prefix(prefix) else {
            continue;
        };

        let mut parts = Vec::new();
        let remainder = remainder.trim();
        if !remainder.is_empty() {
            if let Some(before_close) = remainder.strip_suffix('}') {
                parts.push(before_close.trim());
                return normalize_readability_title(&parts.join(" "));
            }
            parts.push(remainder);
        }

        for next_line in lines.iter().skip(index + 1) {
            let trimmed = next_line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if looks_like_pdf_wrapper_line(trimmed) && parts.is_empty() {
                break;
            }
            if trimmed == "}" {
                break;
            }
            if let Some(before_close) = trimmed.strip_suffix('}') {
                parts.push(before_close.trim());
                break;
            }
            parts.push(trimmed);
        }

        if let Some(title) = normalize_readability_title(&parts.join(" ")) {
            return Some(title);
        }
    }

    None
}

fn strip_inline_pdf_title_command(value: &str) -> &str {
    for prefix in ["\\section*{", "\\section{", "\\title{"] {
        if let Some(stripped) = value.strip_prefix(prefix) {
            return stripped;
        }
    }
    value
}

fn looks_like_pdf_wrapper_line(value: &str) -> bool {
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();
    trimmed == "{"
        || trimmed == "}"
        || trimmed == "\\maketitle"
        || matches!(
            lower.as_str(),
            "\\title{" | "\\author{" | "\\begin{abstract}" | "\\end{abstract}"
        )
        || lower.starts_with("\\begin{")
        || lower.starts_with("\\end{")
        || (trimmed.starts_with('\\') && trimmed.ends_with('{') && !trimmed.contains(' '))
}

fn normalize_readability_title(raw: &str) -> Option<String> {
    let normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let raw_trimmed = normalized.trim();
    if looks_like_pdf_wrapper_line(raw_trimmed) {
        return None;
    }

    let trimmed = raw_trimmed
        .trim_matches(|ch: char| matches!(ch, '{' | '}' | '"' | '\'' | '“' | '”'))
        .trim();
    if trimmed.len() < 2 || looks_like_pdf_wrapper_line(trimmed) {
        return None;
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("/uploads/")
    {
        return None;
    }

    if trimmed
        .chars()
        .all(|ch| !ch.is_alphanumeric() || ch.is_ascii_digit())
    {
        return None;
    }

    Some(trimmed.chars().take(240).collect())
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
#[path = "../../tests/unit/processors_readability_fetch.rs"]
mod tests;
