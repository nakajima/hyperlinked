use super::*;
use std::collections::VecDeque;

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
        classify_source_kind(Some("application/pdf; charset=binary"), b"not-a-pdf"),
        SnapshotSourceKind::Pdf
    );
    assert_eq!(
        classify_source_kind(Some("application/pdf"), b"%PDF-1.7"),
        SnapshotSourceKind::Pdf
    );
    assert_eq!(
        classify_source_kind(None, b"%PDF-1.6\n%"),
        SnapshotSourceKind::Pdf
    );
}

#[test]
fn classifies_html_and_unsupported_sources() {
    assert_eq!(
        classify_source_kind(Some("text/html; charset=utf-8"), b"<html></html>"),
        SnapshotSourceKind::Html
    );
    assert_eq!(
        classify_source_kind(Some("text/html"), b"<html></html>"),
        SnapshotSourceKind::Html
    );
    assert_eq!(
        classify_source_kind(Some("application/json"), b"{\"ok\":true}"),
        SnapshotSourceKind::Unsupported
    );
}

#[test]
fn detects_likely_pdf_urls() {
    let arxiv = Url::parse("https://arxiv.org/pdf/2602.11988").expect("valid arxiv url");
    assert!(is_likely_pdf_url(&arxiv));

    let suffix = Url::parse("https://example.com/files/paper.PDF").expect("valid pdf suffix url");
    assert!(is_likely_pdf_url(&suffix));
}

#[test]
fn does_not_detect_non_pdf_urls_as_likely_pdf() {
    let html = Url::parse("https://example.com/posts/123").expect("valid html url");
    assert!(!is_likely_pdf_url(&html));

    let query_hint =
        Url::parse("https://example.com/download?format=pdf").expect("valid query url");
    assert!(!is_likely_pdf_url(&query_hint));
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
fn skips_screenshot_capture_for_pdf_sources() {
    assert!(should_skip_screenshot_capture_for_source(
        &HyperlinkArtifactKind::PdfSource
    ));
    assert!(!should_skip_screenshot_capture_for_source(
        &HyperlinkArtifactKind::SnapshotWarc
    ));
}

#[test]
fn defaults_hyperlink_source_type_to_unknown_when_unset() {
    let active = hyperlink::ActiveModel::default();
    assert_eq!(hyperlink_source_type(&active), HyperlinkSourceType::Unknown);
}

#[test]
fn absolute_http_url_detection_rejects_relative_paths() {
    assert!(is_absolute_http_or_https_url(
        "https://example.com/file.pdf"
    ));
    assert!(is_absolute_http_or_https_url("http://example.com/file.pdf"));
    assert!(!is_absolute_http_or_https_url("/uploads/1/file.pdf"));
    assert!(!is_absolute_http_or_https_url("file.pdf"));
}

#[test]
fn pdf_thumbnail_render_rejects_invalid_payloads() {
    let result = build_pdf_thumbnail_from_source(b"not-a-pdf", 400);
    assert!(result.is_err());
}

#[test]
fn dark_thumbnail_transform_inverts_light_thumbnail_payload() {
    let light_image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        2,
        2,
        image::Rgba([255, 255, 255, 255]),
    ));
    let light_webp =
        encode_webp_from_dynamic_image(&light_image).expect("light image should encode");
    let dark_webp = build_dark_thumbnail_from_light_thumbnail(&light_webp)
        .expect("dark thumbnail transform should succeed");

    let decoded_dark = image::load_from_memory(&dark_webp)
        .expect("dark thumbnail should decode")
        .to_rgb8();
    let pixel = decoded_dark.get_pixel(0, 0).0;
    assert!(pixel[0] < 80);
    assert!(pixel[1] < 80);
    assert!(pixel[2] < 80);
}

#[test]
fn dark_screenshot_variant_sets_prefers_color_scheme_media_feature() {
    let features = screenshot_variant_media_features(ScreenshotVariant::Dark)
        .expect("dark variant should define media features");
    assert_eq!(features.len(), 1);
    assert_eq!(features[0].name, "prefers-color-scheme");
    assert_eq!(features[0].value, "dark");
}

#[test]
fn light_screenshot_variant_does_not_set_media_features() {
    assert!(screenshot_variant_media_features(ScreenshotVariant::Light).is_none());
}

#[test]
fn dark_screenshot_variant_enables_auto_dark_mode_override() {
    assert!(screenshot_variant_auto_dark_mode_enabled(
        ScreenshotVariant::Dark
    ));
}

#[test]
fn light_screenshot_variant_disables_auto_dark_mode_override() {
    assert!(!screenshot_variant_auto_dark_mode_enabled(
        ScreenshotVariant::Light
    ));
}

#[test]
fn warns_when_dark_capture_matches_light_payload() {
    let warning = dark_capture_matches_light_warning(b"same", b"same");
    assert!(warning.is_some());
}

#[test]
fn does_not_warn_when_dark_capture_differs_from_light_payload() {
    let warning = dark_capture_matches_light_warning(b"light", b"dark");
    assert!(warning.is_none());
}

#[test]
fn chromium_launch_args_include_dark_flags_for_dark_variant() {
    let args = chromium_launch_args(Some(ScreenshotVariant::Dark), 5_000);
    assert!(args.iter().any(|arg| arg == "--force-dark-mode"));
    assert!(
        args.iter()
            .any(|arg| arg == "--enable-features=WebContentsForceDark")
    );
}

#[test]
fn chromium_launch_args_exclude_dark_flags_for_light_variant() {
    let args = chromium_launch_args(Some(ScreenshotVariant::Light), 5_000);
    assert!(!args.iter().any(|arg| arg == "--force-dark-mode"));
    assert!(
        !args
            .iter()
            .any(|arg| arg == "--enable-features=WebContentsForceDark")
    );
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
fn screenshot_retry_backoff_grows() {
    let first = screenshot_retry_backoff_delay(1);
    let second = screenshot_retry_backoff_delay(2);
    assert!(second >= first);
}

#[test]
fn screenshot_retryable_error_classifier_defaults_to_retry() {
    assert!(screenshot_capture_error_retryable(
        "failed to read chromium devtools response for `Runtime.evaluate`: connection reset"
    ));
    assert!(screenshot_capture_error_retryable(
        "failed to read chromium devtools response for `Runtime.evaluate`: WebSocket protocol error: Connection reset without closing handshake"
    ));
}

#[test]
fn screenshot_retryable_error_classifier_rejects_deterministic_errors() {
    assert!(!screenshot_capture_error_retryable(
        "chromium page-height evaluation returned a non-numeric result"
    ));
    assert!(!screenshot_capture_error_retryable(
        EXACT_HEIGHT_EMPTY_PAYLOAD_ERROR
    ));
    assert!(!screenshot_capture_error_retryable(
        "failed to decode chromium screenshot payload: Invalid byte"
    ));
    assert!(!screenshot_capture_error_retryable(
        "failed to launch chromium browser: No such file or directory (os error 2)"
    ));
    assert!(!screenshot_capture_error_retryable(
        "timed out waiting for chromium devtools endpoint"
    ));
    assert!(!screenshot_capture_error_retryable(
        "mkdir: cannot create directory '/run/user/0': Permission denied"
    ));
}

#[test]
fn exact_height_retryable_error_classifier_rejects_connection_reset() {
    assert!(!exact_height_capture_error_retryable(
        "failed to read chromium devtools response for `Emulation.setDeviceMetricsOverride`: WebSocket protocol error: Connection reset without closing handshake"
    ));
}

#[test]
fn exact_height_skips_fixed_viewport_fallback_for_startup_errors() {
    assert!(should_skip_fixed_viewport_fallback_for_exact_error(
        "failed to launch chromium browser: No such file or directory (os error 2)"
    ));
    assert!(should_skip_fixed_viewport_fallback_for_exact_error(
        "timed out waiting for chromium devtools endpoint"
    ));
    assert!(!should_skip_fixed_viewport_fallback_for_exact_error(
        "chromium page-height evaluation returned a non-numeric result"
    ));
}

#[test]
fn exact_height_state_disables_after_empty_payload_error() {
    let mut state = ExactHeightCaptureState::from_enabled(true);
    assert!(state.should_try_exact_height());

    let disabled = state.disable_for_job_on_error(EXACT_HEIGHT_EMPTY_PAYLOAD_ERROR);
    assert!(disabled);
    assert!(!state.should_try_exact_height());
}

#[test]
fn exact_height_state_keeps_exact_height_for_other_errors() {
    let mut state = ExactHeightCaptureState::from_enabled(true);
    assert!(state.should_try_exact_height());

    let disabled =
        state.disable_for_job_on_error("exact-height screenshot capture timed out after 20s");
    assert!(!disabled);
    assert!(state.should_try_exact_height());
}

#[test]
fn screenshot_failure_artifact_serializes_font_diagnostics() {
    let artifact = ScreenshotFailureArtifact {
        source_url: "https://example.com".to_string(),
        failed_at: "2026-02-26T00:00:00Z".to_string(),
        errors: vec!["screenshot failed".to_string()],
        chromium_path: "chromium".to_string(),
        timeout_secs: 20,
        attempts: None,
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

#[test]
fn screenshot_failure_artifact_serializes_attempts_when_present() {
    let artifact = ScreenshotFailureArtifact {
        source_url: "https://example.com".to_string(),
        failed_at: "2026-02-26T00:00:00Z".to_string(),
        errors: vec!["screenshot failed".to_string()],
        chromium_path: "chromium".to_string(),
        timeout_secs: 20,
        attempts: Some(vec![ScreenshotCaptureAttempt {
            attempt: 1,
            stage: ScreenshotCaptureStage::ExactHeight,
            error: "connection reset".to_string(),
            retryable: true,
        }]),
        font_diagnostics: None,
    };

    let value = serde_json::to_value(&artifact).expect("artifact should serialize");
    assert!(value.get("attempts").is_some());
}

struct FakeBrowserLifecycle {
    close_result: Result<(), String>,
    wait_results: VecDeque<Result<Option<()>, String>>,
    kill_result: Result<bool, String>,
    close_calls: usize,
    wait_calls: usize,
    kill_calls: usize,
}

impl FakeBrowserLifecycle {
    fn with_results(
        close_result: Result<(), String>,
        wait_results: Vec<Result<Option<()>, String>>,
        kill_result: Result<bool, String>,
    ) -> Self {
        Self {
            close_result,
            wait_results: wait_results.into_iter().collect(),
            kill_result,
            close_calls: 0,
            wait_calls: 0,
            kill_calls: 0,
        }
    }
}

#[async_trait::async_trait]
impl BrowserLifecycle for FakeBrowserLifecycle {
    async fn close_browser(&mut self) -> Result<(), String> {
        self.close_calls += 1;
        self.close_result.clone()
    }

    async fn wait_for_exit(&mut self) -> Result<Option<()>, String> {
        self.wait_calls += 1;
        self.wait_results.pop_front().unwrap_or(Ok(Some(())))
    }

    async fn kill_browser(&mut self) -> Result<bool, String> {
        self.kill_calls += 1;
        self.kill_result.clone()
    }
}

#[tokio::test]
async fn shutdown_browser_lifecycle_reaps_after_close_without_kill() {
    let mut lifecycle = FakeBrowserLifecycle::with_results(Ok(()), vec![Ok(Some(()))], Ok(true));
    let warnings = shutdown_browser_lifecycle(&mut lifecycle, Path::new("/tmp/profile")).await;

    assert!(warnings.is_empty());
    assert_eq!(lifecycle.close_calls, 1);
    assert_eq!(lifecycle.wait_calls, 1);
    assert_eq!(lifecycle.kill_calls, 0);
}

#[tokio::test]
async fn shutdown_browser_lifecycle_kills_when_close_wait_fails() {
    let mut lifecycle = FakeBrowserLifecycle::with_results(
        Err("close failed".to_string()),
        vec![Err("wait after close failed".to_string()), Ok(Some(()))],
        Ok(true),
    );
    let warnings = shutdown_browser_lifecycle(&mut lifecycle, Path::new("/tmp/profile")).await;

    assert_eq!(lifecycle.close_calls, 1);
    assert_eq!(lifecycle.wait_calls, 2);
    assert_eq!(lifecycle.kill_calls, 1);
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("failed to close chromium browser"))
    );
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("wait after close failed"))
    );
    assert!(
        !warnings
            .iter()
            .any(|warning| warning.contains("not confirmed as reaped"))
    );
}

#[tokio::test]
async fn shutdown_browser_lifecycle_warns_when_reap_not_confirmed() {
    let mut lifecycle = FakeBrowserLifecycle::with_results(
        Ok(()),
        vec![
            Err("wait close failed".to_string()),
            Err("wait kill failed".to_string()),
        ],
        Ok(false),
    );
    let warnings = shutdown_browser_lifecycle(&mut lifecycle, Path::new("/tmp/profile")).await;

    assert_eq!(lifecycle.close_calls, 1);
    assert_eq!(lifecycle.wait_calls, 2);
    assert_eq!(lifecycle.kill_calls, 1);
    assert!(warnings.iter().any(|warning| {
        warning.contains("kill was skipped because no child process handle was available")
    }));
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("not confirmed as reaped"))
    );
}

#[tokio::test]
async fn remove_directory_with_retries_removes_nested_directories() {
    use std::{fs, time::SystemTime};

    let unique = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("clock should be monotonic")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("hyperlinked-remove-dir-test-{unique}"));
    let nested = root.join("nested");

    fs::create_dir_all(&nested).expect("nested test directory should be created");
    fs::write(nested.join("child.txt"), b"ok").expect("test file should be written");

    remove_directory_with_retries(root.as_path(), 2, 1)
        .await
        .expect("directory removal should succeed");
    assert!(!root.exists());
}
