use std::collections::HashSet;

use reqwest::Url;

const COMMON_TITLE_SEPARATORS: [&str; 8] =
    [" | ", " - ", " — ", " – ", " :: ", " · ", " » ", " • "];

pub fn strip_site_affixes(title: &str, primary_url: &str, fallback_url: &str) -> String {
    let normalized = title.trim();
    if normalized.is_empty() {
        return String::new();
    }

    let segments = split_title_segments(normalized);
    if segments.len() < 2 {
        return normalized.to_string();
    }

    let host_tokens = extract_host_tokens(primary_url)
        .into_iter()
        .chain(extract_host_tokens(fallback_url))
        .collect::<HashSet<_>>();

    if segments.len() > 2 {
        let mut start = 0usize;
        let mut end = segments.len();

        while start < end && is_site_segment(&segments[start], &host_tokens) {
            start += 1;
        }
        while end > start && is_site_segment(&segments[end - 1], &host_tokens) {
            end -= 1;
        }

        if start > 0 || end < segments.len() {
            let trimmed = segments[start..end].to_vec();
            if !trimmed.is_empty() {
                return trimmed.join(" - ");
            }
        }
    }

    let first = &segments[0];
    let last = &segments[segments.len() - 1];
    let first_is_site = is_site_segment(first, &host_tokens);
    let last_is_site = is_site_segment(last, &host_tokens);

    if first_is_site && !last_is_site {
        return last.clone();
    }
    if last_is_site && !first_is_site {
        return first.clone();
    }

    normalized.to_string()
}

fn split_title_segments(value: &str) -> Vec<String> {
    let mut normalized = value.to_string();
    for separator in COMMON_TITLE_SEPARATORS {
        normalized = normalized.replace(separator, "|");
    }

    if !normalized.contains('|') {
        return vec![value.to_string()];
    }

    let parts = normalized
        .split('|')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    if parts.len() < 2 {
        vec![value.to_string()]
    } else {
        parts
    }
}

fn extract_host_tokens(url: &str) -> Vec<String> {
    let Some(host) = Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(|value| value.to_ascii_lowercase()))
    else {
        return Vec::new();
    };

    let host = host
        .strip_prefix("www.")
        .map(ToString::to_string)
        .unwrap_or(host);
    let mut tokens = vec![host.clone()];

    let parts = host.split('.').collect::<Vec<_>>();
    if let Some(first_label) = parts.first().copied().filter(|value| !value.is_empty()) {
        tokens.push(first_label.to_string());
    }

    if parts.len() >= 2 {
        let registrable = format!("{}.{}", parts[parts.len() - 2], parts[parts.len() - 1]);
        tokens.push(registrable);
    }

    tokens
}

fn is_site_segment(segment: &str, host_tokens: &HashSet<String>) -> bool {
    let trimmed = segment.trim();
    if trimmed.is_empty() {
        return false;
    }

    if looks_like_domain_fragment(trimmed) {
        return true;
    }

    let compact_segment = compact_for_compare(trimmed);
    if compact_segment.is_empty() {
        return false;
    }

    host_tokens.iter().any(|token| {
        let compact_token = compact_for_compare(token);
        if compact_token.is_empty() {
            return false;
        }

        compact_segment == compact_token
    })
}

fn compact_for_compare(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| ch.to_lowercase())
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect()
}

fn looks_like_domain_fragment(value: &str) -> bool {
    if value.contains(char::is_whitespace) || !value.contains('.') {
        return false;
    }

    let labels = value.split('.').collect::<Vec<_>>();
    if labels.len() < 2 {
        return false;
    }

    let Some(tld) = labels.last() else {
        return false;
    };
    if tld.len() < 2 || !tld.chars().all(|ch| ch.is_ascii_alphabetic()) {
        return false;
    }

    value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_' || ch == '/')
}

#[cfg(test)]
mod tests {
    use super::strip_site_affixes;

    #[test]
    fn strips_trailing_site_name_when_host_matches() {
        let cleaned = strip_site_affixes(
            "Understanding Rust Lifetimes | Example.com",
            "https://example.com/rust",
            "https://example.com/rust",
        );
        assert_eq!(cleaned, "Understanding Rust Lifetimes");
    }

    #[test]
    fn strips_leading_site_name_when_host_matches() {
        let cleaned = strip_site_affixes(
            "Example.com - Understanding Rust Lifetimes",
            "https://example.com/rust",
            "https://example.com/rust",
        );
        assert_eq!(cleaned, "Understanding Rust Lifetimes");
    }

    #[test]
    fn strips_site_edges_from_multi_segment_titles() {
        let cleaned = strip_site_affixes(
            "Example.com | Understanding Rust Lifetimes | Programming",
            "https://example.com/rust",
            "https://example.com/rust",
        );
        assert_eq!(cleaned, "Understanding Rust Lifetimes - Programming");
    }

    #[test]
    fn keeps_non_site_titles_with_separator() {
        let cleaned = strip_site_affixes(
            "Rust - The Book",
            "https://doc.rust-lang.org/book",
            "https://doc.rust-lang.org/book",
        );
        assert_eq!(cleaned, "Rust - The Book");
    }
}
