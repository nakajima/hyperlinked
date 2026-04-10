use super::*;
use sea_orm::{ActiveModelTrait, ActiveValue::Set};

use crate::{
    entity::{
        hyperlink,
        hyperlink_artifact::{self, HyperlinkArtifactKind},
        hyperlink_processing_job::{self, HyperlinkProcessingJobKind, HyperlinkProcessingJobState},
    },
    test_support,
};

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
fn rejects_frameset_documents_before_dom_smoothie_panics() {
    let html = concat!(
        "<!DOCTYPE html PUBLIC \"-//W3C//DTD HTML 4.01 Frameset//EN\" ",
        "\"http://www.w3.org/TR/html4/frameset.dtd\">",
        "<html><head><title></title></head>",
        "<frameset rows=\"100%, *\">",
        "<frame src=\"https://example.com/frame\" name=\"mainwindow\">",
        "</frameset>",
        "<noframes><body><p>fallback</p></body></noframes>",
        "</html>"
    );

    let error = extract_from_html(html).expect_err("frameset documents should be rejected");
    assert_eq!(error.0, "readability_parse");
    assert!(error.1.contains("frameset"));
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

#[test]
fn infers_pdf_title_from_markdown_heading() {
    assert_eq!(
        infer_pdf_title_from_markdown("# Quarterly Report\n\nBody text").as_deref(),
        Some("Quarterly Report")
    );
}

#[test]
fn ignores_non_title_pdf_markdown_lines() {
    assert_eq!(infer_pdf_title_from_markdown("---\n\n2024\n"), None);
}

struct FakePdfExtractor {
    name: &'static str,
    result: Result<PdfExtraction, String>,
}

#[async_trait::async_trait]
impl PdfTextExtractor for FakePdfExtractor {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn extract(&self, _payload: &[u8]) -> Result<PdfExtraction, String> {
        self.result.clone()
    }
}

#[tokio::test]
async fn extracts_pdf_text_and_metadata() {
    let (text_payload, meta_payload, readability_title) = extract_from_pdf(
        &FakePdfExtractor {
            name: "fake",
            result: Ok(PdfExtraction {
                markdown: "sample text".to_string(),
                page_count: Some(2),
                title: Some("Readable Title".to_string()),
            }),
        },
        "Doc",
        "https://example.com/report.pdf",
        b"%PDF",
    )
    .await
    .expect("pdf extraction should succeed");

    assert_eq!(
        String::from_utf8(text_payload).expect("text payload should decode"),
        "sample text"
    );
    let meta: serde_json::Value =
        serde_json::from_slice(&meta_payload).expect("meta should decode");
    assert_eq!(meta["source_format"], "pdf");
    assert_eq!(meta["title"], "Readable Title");
    assert_eq!(meta["pdf_page_count"], 2);
    assert_eq!(meta["pdf_extractor"], "fake");
    assert_eq!(readability_title.as_deref(), Some("Readable Title"));
}

#[tokio::test]
async fn falls_back_to_local_extractor_when_primary_fails() {
    let (text_payload, meta_payload, readability_title) = extract_from_pdf_with_fallback(
        Some(&FakePdfExtractor {
            name: "mathpix",
            result: Err("mathpix failed".to_string()),
        }),
        &FakePdfExtractor {
            name: "pdf_extract",
            result: Ok(PdfExtraction {
                markdown: "fallback text".to_string(),
                page_count: Some(3),
                title: Some("Fallback Title".to_string()),
            }),
        },
        "Doc",
        "https://example.com/report.pdf",
        b"%PDF",
    )
    .await
    .expect("fallback extractor should succeed");

    assert_eq!(
        String::from_utf8(text_payload).expect("text payload should decode"),
        "fallback text"
    );
    let meta: serde_json::Value =
        serde_json::from_slice(&meta_payload).expect("meta should decode");
    assert_eq!(meta["pdf_extractor"], "pdf_extract");
    assert_eq!(readability_title.as_deref(), Some("Fallback Title"));
}

#[tokio::test]
async fn returns_combined_error_when_primary_and_fallback_fail() {
    let error = extract_from_pdf_with_fallback(
        Some(&FakePdfExtractor {
            name: "mathpix",
            result: Err("primary error".to_string()),
        }),
        &FakePdfExtractor {
            name: "pdf_extract",
            result: Err("fallback error".to_string()),
        },
        "Doc",
        "https://example.com/report.pdf",
        b"%PDF",
    )
    .await
    .expect_err("both extractors should fail");

    assert_eq!(error.0, "pdf_extract");
    assert!(error.1.contains("mathpix failed: primary error"));
    assert!(
        error
            .1
            .contains("fallback pdf_extract failed: fallback error")
    );
}

#[tokio::test]
async fn readability_process_updates_pdf_hyperlink_title_from_extracted_title() {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_schema(&connection).await;

    let now = now_utc();
    let hyperlink = hyperlink::ActiveModel {
        id: Set(1),
        title: Set("document".to_string()),
        url: Set("/uploads/1/document.pdf".to_string()),
        raw_url: Set("/uploads/1/document.pdf".to_string()),
        source_type: Set(hyperlink::HyperlinkSourceType::Pdf),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(&connection)
    .await
    .expect("hyperlink should insert");

    hyperlink_processing_job::ActiveModel {
        id: Set(7),
        hyperlink_id: Set(hyperlink.id),
        kind: Set(HyperlinkProcessingJobKind::Readability),
        state: Set(HyperlinkProcessingJobState::Running),
        error_message: Set(None),
        queued_at: Set(now),
        started_at: Set(Some(now)),
        finished_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&connection)
    .await
    .expect("job should insert");

    hyperlink_artifact::ActiveModel {
        hyperlink_id: Set(hyperlink.id),
        job_id: Set(None),
        kind: Set(HyperlinkArtifactKind::PdfSource),
        payload: Set(b"%PDF".to_vec()),
        storage_path: Set(None),
        storage_backend: Set(None),
        checksum_sha256: Set(None),
        content_type: Set("application/pdf".to_string()),
        size_bytes: Set(4),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(&connection)
    .await
    .expect("pdf source should insert");

    let mut active_hyperlink: hyperlink::ActiveModel = hyperlink.into();
    let mut fetcher = ReadabilityFetcher::with_pdf_extractors(
        7,
        Box::new(FakePdfExtractor {
            name: "fake",
            result: Ok(PdfExtraction {
                markdown: "# Readable Upload Title\n\nBody".to_string(),
                page_count: Some(1),
                title: Some("Readable Upload Title".to_string()),
            }),
        }),
        None,
    );

    let output = fetcher
        .process(&mut active_hyperlink, &connection)
        .await
        .expect("readability should succeed");

    assert!(output.text_artifact_id.is_some());
    assert!(output.meta_artifact_id.is_some());
    assert_eq!(active_hyperlink.title.as_ref(), "Readable Upload Title");
}

#[test]
fn recognizes_mathpix_status_values() {
    assert!(is_mathpix_completed_status("completed"));
    assert!(is_mathpix_completed_status("success"));
    assert!(is_mathpix_failed_status("failed"));
    assert!(is_mathpix_failed_status("error"));
    assert!(!is_mathpix_completed_status("processing"));
    assert!(!is_mathpix_failed_status("processing"));
}

#[test]
fn infers_mathpix_page_count() {
    let value = serde_json::json!({ "num_pages": 12 });
    assert_eq!(infer_mathpix_page_count(&value), Some(12));

    let value = serde_json::json!({ "num_pages_total": 7 });
    assert_eq!(infer_mathpix_page_count(&value), Some(7));

    let value = serde_json::json!({});
    assert_eq!(infer_mathpix_page_count(&value), None);
}
