use serde_json::json;

use super::*;

#[test]
fn mode_is_disabled_when_api_token_missing() {
    let mode = parse_mode(MathpixEnvValues::default());
    let status = mode.status();
    assert!(!status.enabled);
    assert!(status.reason.contains(MATHPIX_API_TOKEN_ENV));
}

#[test]
fn mode_is_disabled_when_app_id_missing() {
    let mode = parse_mode(MathpixEnvValues {
        api_token: Some("test-key".to_string()),
        app_id: None,
        ..Default::default()
    });
    let status = mode.status();
    assert!(!status.enabled);
    assert!(status.reason.contains(MATHPIX_APP_ID_ENV));
    assert!(mode.disabled_missing_app_id());
}

#[test]
fn mode_is_enabled_when_required_env_is_present() {
    let mode = parse_mode(MathpixEnvValues {
        api_token: Some("  test-key  ".to_string()),
        app_id: Some("  app-id  ".to_string()),
        ..Default::default()
    });

    match mode {
        MathpixMode::Enabled(config) => {
            assert_eq!(config.app_key, "test-key");
            assert_eq!(config.app_id, "app-id");
            assert_eq!(config.request_timeout, Duration::from_secs(30));
            assert_eq!(config.poll_interval, Duration::from_millis(1_000));
            assert_eq!(config.poll_timeout, Duration::from_secs(90));
        }
        MathpixMode::Disabled(status) => {
            panic!("expected enabled mathpix mode, got {}", status.reason)
        }
    }
}

#[test]
fn mode_clamps_mathpix_timing_values() {
    let mode = parse_mode(MathpixEnvValues {
        api_token: Some("test-key".to_string()),
        app_id: Some("app-id".to_string()),
        timeout_secs: Some("999".to_string()),
        poll_interval_ms: Some("1".to_string()),
        poll_timeout_secs: Some("9".to_string()),
    });

    match mode {
        MathpixMode::Enabled(config) => {
            assert_eq!(config.request_timeout, Duration::from_secs(120));
            assert_eq!(config.poll_interval, Duration::from_millis(250));
            assert_eq!(config.poll_timeout, Duration::from_secs(10));
        }
        MathpixMode::Disabled(status) => {
            panic!("expected enabled mathpix mode, got {}", status.reason)
        }
    }
}

#[test]
fn parse_usage_records_reads_usage_rows() {
    let body = json!({
        "ocr_usage": [
            {"usage_type": "image", "count": 11},
            {"usage_type": "pdf_pages", "count": "7"},
            {"usage_type": "ignored-no-count"},
            {"usage_type": "", "count": 3},
            {"count": 99}
        ]
    })
    .to_string();

    let parsed = parse_usage_records(body.as_str()).expect("usage rows should parse");
    assert_eq!(
        parsed,
        vec![
            UsageRecord {
                usage_type: "image".to_string(),
                count: 11
            },
            UsageRecord {
                usage_type: "pdf_pages".to_string(),
                count: 7
            }
        ]
    );
}

#[test]
fn summarize_usage_window_applies_tiers_for_image_and_pdf() {
    let summary = summarize_usage_window(&[
        UsageRecord {
            usage_type: "image".to_string(),
            count: 1_200,
        },
        UsageRecord {
            usage_type: "pdf-async".to_string(),
            count: 5_100,
        },
    ]);

    assert_eq!(summary.total_requests, 6_300);
    let expected =
        (1_000.0 * 0.002 + 200.0 * 0.0015) + (1_000.0 * 0.005 + 4_000.0 * 0.004 + 100.0 * 0.003);
    assert!((summary.estimated_cost_usd - expected).abs() < f64::EPSILON);
    assert_eq!(summary.breakdown.len(), 2);
    assert_eq!(summary.breakdown[0].usage_type, "image");
    assert_eq!(
        summary.breakdown[0].cost_class,
        MathpixUsageCostClass::ImageRequest
    );
    assert_eq!(summary.breakdown[1].usage_type, "pdf-async");
    assert_eq!(
        summary.breakdown[1].cost_class,
        MathpixUsageCostClass::PdfRequest
    );
}

#[test]
fn unknown_usage_types_are_reported_sorted() {
    let window = summarize_usage_window(&[
        UsageRecord {
            usage_type: "zeta".to_string(),
            count: 1,
        },
        UsageRecord {
            usage_type: "image".to_string(),
            count: 1,
        },
        UsageRecord {
            usage_type: "alpha".to_string(),
            count: 1,
        },
        UsageRecord {
            usage_type: "zeta".to_string(),
            count: 1,
        },
    ]);
    let unknown = unknown_usage_types(&window);
    assert_eq!(unknown, vec!["alpha".to_string(), "zeta".to_string()]);
}

#[test]
fn normalize_usage_type_handles_separators() {
    assert_eq!(normalize_usage_type(" PDF_Pages "), "pdf-pages");
    assert_eq!(normalize_usage_type("image/async"), "image-async");
}

#[test]
fn parse_usage_count_accepts_decimal_strings() {
    let row = json!({
        "count": "7.49"
    });
    assert_eq!(parse_usage_count(&row), Some(7));
}
