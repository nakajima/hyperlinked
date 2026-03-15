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
    let (text_payload, meta_payload) = extract_from_pdf(
        &FakePdfExtractor {
            name: "fake",
            result: Ok(PdfExtraction {
                markdown: "sample text".to_string(),
                page_count: Some(2),
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
    assert_eq!(meta["title"], "Doc");
    assert_eq!(meta["pdf_page_count"], 2);
    assert_eq!(meta["pdf_extractor"], "fake");
}

#[tokio::test]
async fn falls_back_to_local_extractor_when_primary_fails() {
    let (text_payload, meta_payload) = extract_from_pdf_with_fallback(
        Some(&FakePdfExtractor {
            name: "mathpix",
            result: Err("mathpix failed".to_string()),
        }),
        &FakePdfExtractor {
            name: "pdf_extract",
            result: Ok(PdfExtraction {
                markdown: "fallback text".to_string(),
                page_count: Some(3),
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
