use super::*;

#[test]
fn drops_known_tracking_params() {
    let canonicalized = canonicalize_submitted_url(
        "https://example.com/articles/rust?utm_source=newsletter&fbclid=abc123&q=rust",
    )
    .expect("url should canonicalize");
    assert_eq!(
        canonicalized.canonical_url,
        "https://example.com/articles/rust?q=rust"
    );
}

#[test]
fn drops_tracking_params_case_insensitively() {
    let canonicalized = canonicalize_submitted_url(
        "https://example.com/articles/rust?UtM_Source=abc&GCLID=1&Page=2",
    )
    .expect("url should canonicalize");
    assert_eq!(
        canonicalized.canonical_url,
        "https://example.com/articles/rust?Page=2"
    );
}

#[test]
fn keeps_non_tracking_params_for_hosts_without_strict_rules() {
    let canonicalized =
        canonicalize_submitted_url("https://example.com/path?foo=bar&x=1&utm_medium=email")
            .expect("url should canonicalize");
    assert_eq!(
        canonicalized.canonical_url,
        "https://example.com/path?foo=bar&x=1"
    );
}

#[test]
fn applies_host_strict_keep_rules() {
    let canonicalized = canonicalize_submitted_url(
        "https://www.youtube.com/watch?v=abc123&feature=share&foo=bar&list=xyz",
    )
    .expect("url should canonicalize");
    assert_eq!(
        canonicalized.canonical_url,
        "https://www.youtube.com/watch?v=abc123&list=xyz"
    );
}

#[test]
fn removes_fragment_and_default_port() {
    let canonicalized = canonicalize_submitted_url("https://example.com:443/path#intro")
        .expect("url should canonicalize");
    assert_eq!(canonicalized.canonical_url, "https://example.com/path");
}

#[test]
fn renders_root_without_trailing_slash() {
    let canonicalized =
        canonicalize_submitted_url("https://example.com/").expect("url should canonicalize");
    assert_eq!(canonicalized.canonical_url, "https://example.com");
}

#[test]
fn preserves_param_order_and_duplicates_for_kept_params() {
    let canonicalized =
        canonicalize_submitted_url("https://example.com/search?a=1&utm_source=x&a=2&b=3&a=4")
            .expect("url should canonicalize");
    assert_eq!(
        canonicalized.canonical_url,
        "https://example.com/search?a=1&a=2&b=3&a=4"
    );
}

#[test]
fn rejects_invalid_url() {
    let error = canonicalize_submitted_url("not a url").expect_err("url should fail");
    assert!(error.contains("invalid url"));
}

#[test]
fn rejects_non_http_urls() {
    let error =
        canonicalize_submitted_url("mailto:test@example.com").expect_err("mailto should fail");
    assert_eq!(error, "url must use http or https");
}
