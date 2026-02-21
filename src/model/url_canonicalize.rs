use reqwest::Url;

const TRACKING_EXACT_PARAMS: &[&str] = &[
    "fbclid", "gclid", "dclid", "gbraid", "wbraid", "msclkid", "mc_cid", "mc_eid", "igshid",
    "yclid", "_hsenc", "_hsmi",
];
const TRACKING_PREFIX_PARAMS: &[&str] = &["utm_"];

const GLOBAL_SAFE_MEANINGFUL_PARAMS: &[&str] = &[
    "q", "query", "search", "page", "p", "sort", "order", "lang", "locale", "id", "v", "t", "list",
];

#[derive(Clone, Copy, Debug)]
struct HostRule {
    host: &'static str,
    path_prefix: Option<&'static str>,
    keep_exact: &'static [&'static str],
    keep_prefix: &'static [&'static str],
}

// Keep this list intentionally small and explicit. Add entries as we learn
// host-specific needs from real URLs in this app.
const HOST_RULES: &[HostRule] = &[
    HostRule {
        host: "youtube.com",
        path_prefix: Some("/watch"),
        keep_exact: &["v", "list", "t", "start", "index"],
        keep_prefix: &[],
    },
    HostRule {
        host: "youtu.be",
        path_prefix: None,
        keep_exact: &["t", "start"],
        keep_prefix: &[],
    },
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalizedUrl {
    pub raw_url: String,
    pub canonical_url: String,
}

pub fn canonicalize_submitted_url(input: &str) -> Result<CanonicalizedUrl, String> {
    let raw_url = input.trim();
    if raw_url.is_empty() {
        return Err("url must not be empty".to_string());
    }

    let mut url = Url::parse(raw_url).map_err(|err| format!("invalid url: {err}"))?;
    match url.scheme() {
        "http" | "https" => {}
        _ => return Err("url must use http or https".to_string()),
    }

    if (url.scheme() == "http" && url.port() == Some(80))
        || (url.scheme() == "https" && url.port() == Some(443))
    {
        url.set_port(None)
            .map_err(|_| "invalid url: failed to normalize default port".to_string())?;
    }
    url.set_fragment(None);

    if url.path().is_empty() {
        url.set_path("/");
    }

    let host = url
        .host_str()
        .ok_or_else(|| "url must include host".to_string())?
        .to_ascii_lowercase();
    let path = url.path().to_string();
    let host_rules = rules_for_host_and_path(&host, &path);
    let strict_keep_mode = !host_rules.is_empty();

    let kept_pairs = url
        .query_pairs()
        .filter(|(key, _)| !is_tracking_param(key))
        .filter(|(key, _)| should_keep_param(key, strict_keep_mode, &host_rules))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();

    url.set_query(None);
    if !kept_pairs.is_empty() {
        let mut pairs_mut = url.query_pairs_mut();
        for (key, value) in kept_pairs {
            pairs_mut.append_pair(&key, &value);
        }
        drop(pairs_mut);
    }

    Ok(CanonicalizedUrl {
        raw_url: raw_url.to_string(),
        canonical_url: format_canonical_url(&url),
    })
}

fn rules_for_host_and_path(host: &str, path: &str) -> Vec<&'static HostRule> {
    HOST_RULES
        .iter()
        .filter(|rule| host_matches_rule(host, rule.host))
        .filter(|rule| {
            rule.path_prefix
                .is_none_or(|prefix| path.starts_with(prefix))
        })
        .collect()
}

fn host_matches_rule(host: &str, rule_host: &str) -> bool {
    host == rule_host || host.ends_with(&format!(".{rule_host}"))
}

fn should_keep_param(key: &str, strict_keep_mode: bool, host_rules: &[&HostRule]) -> bool {
    if !strict_keep_mode {
        return true;
    }

    is_exact_param_match(key, GLOBAL_SAFE_MEANINGFUL_PARAMS)
        || host_rules.iter().any(|rule| {
            is_exact_param_match(key, rule.keep_exact)
                || is_prefix_param_match(key, rule.keep_prefix)
        })
}

fn is_tracking_param(key: &str) -> bool {
    is_exact_param_match(key, TRACKING_EXACT_PARAMS)
        || is_prefix_param_match(key, TRACKING_PREFIX_PARAMS)
}

fn is_exact_param_match(key: &str, candidates: &[&str]) -> bool {
    let lowered = key.to_ascii_lowercase();
    candidates.iter().any(|candidate| lowered == *candidate)
}

fn is_prefix_param_match(key: &str, prefixes: &[&str]) -> bool {
    let lowered = key.to_ascii_lowercase();
    prefixes.iter().any(|prefix| lowered.starts_with(prefix))
}

fn format_canonical_url(url: &Url) -> String {
    let mut canonical = url.to_string();
    if url.path() == "/" {
        if url.query().is_none() {
            canonical.pop();
        } else {
            canonical = canonical.replacen("/?", "?", 1);
        }
    }
    canonical
}

#[cfg(test)]
mod tests {
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
        let canonicalized = canonicalize_submitted_url("https://example.com/")
            .expect("url should canonicalize");
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
}
