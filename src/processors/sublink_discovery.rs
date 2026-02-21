use std::collections::HashSet;

use reqwest::Url;
use sea_orm::DatabaseConnection;

use crate::{
    entity::{hyperlink, hyperlink_artifact::HyperlinkArtifactKind},
    model::{
        hyperlink as hyperlink_model, hyperlink_artifact, hyperlink_processing_job,
        hyperlink_relation,
    },
    processors::processor::{ProcessingError, Processor},
};

const MAX_SUBLINKS_PER_PARENT: usize = 200;

pub struct SublinkDiscoveryProcessor {
    processing_queue: Option<hyperlink_processing_job::ProcessingQueueSender>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SublinkDiscoveryOutput {
    pub candidates: usize,
    pub created: usize,
    pub linked_existing: usize,
    pub skipped: usize,
}

impl SublinkDiscoveryProcessor {
    pub fn new(processing_queue: Option<hyperlink_processing_job::ProcessingQueueSender>) -> Self {
        Self { processing_queue }
    }
}

impl Processor for SublinkDiscoveryProcessor {
    type Output = SublinkDiscoveryOutput;

    async fn process<'a>(
        &'a mut self,
        hyperlink: &'a mut hyperlink::ActiveModel,
        connection: &'a DatabaseConnection,
    ) -> Result<Self::Output, ProcessingError> {
        let parent_hyperlink_id = *hyperlink.id.as_ref();
        let source_url = hyperlink.url.as_ref().to_string();
        let normalized_source_url =
            hyperlink_model::validate_and_normalize(hyperlink_model::HyperlinkInput {
                title: String::new(),
                url: source_url.clone(),
            })
            .await
            .map(|input| input.url)
            .unwrap_or_else(|_| source_url.clone());

        let Some(readable_text) = hyperlink_artifact::latest_for_hyperlink_kind(
            connection,
            parent_hyperlink_id,
            HyperlinkArtifactKind::ReadableText,
        )
        .await
        .map_err(ProcessingError::DB)?
        else {
            return Ok(SublinkDiscoveryOutput::default());
        };

        let markdown = String::from_utf8_lossy(&readable_text.payload);
        let raw_urls = extract_markdown_urls(&markdown);

        let mut normalized = Vec::new();
        let mut seen = HashSet::new();
        let mut skipped = 0usize;
        for raw in raw_urls {
            let Some(candidate_url) = normalize_candidate_url(&source_url, &raw) else {
                skipped += 1;
                continue;
            };
            let Ok(normalized_input) =
                hyperlink_model::validate_and_normalize(hyperlink_model::HyperlinkInput {
                    title: String::new(),
                    url: candidate_url,
                })
                .await
            else {
                skipped += 1;
                continue;
            };

            if normalized_input.url == normalized_source_url {
                skipped += 1;
                continue;
            }
            if seen.insert(normalized_input.url.clone()) {
                normalized.push(normalized_input);
            }
        }

        if normalized.len() > MAX_SUBLINKS_PER_PARENT {
            skipped += normalized.len() - MAX_SUBLINKS_PER_PARENT;
            normalized.truncate(MAX_SUBLINKS_PER_PARENT);
        }

        let mut created = 0usize;
        let mut linked_existing = 0usize;
        for input in &normalized {
            let child = if let Some(existing) = hyperlink_model::find_by_url(connection, &input.url)
                .await
                .map_err(ProcessingError::DB)?
            {
                linked_existing += 1;
                existing
            } else {
                let inserted = hyperlink_model::insert_discovered(
                    connection,
                    input.clone(),
                    self.processing_queue.as_ref(),
                )
                .await
                .map_err(ProcessingError::DB)?;
                created += 1;
                inserted
            };

            hyperlink_relation::link_parent_child(connection, parent_hyperlink_id, child.id)
                .await
                .map_err(ProcessingError::DB)?;
        }

        Ok(SublinkDiscoveryOutput {
            candidates: normalized.len(),
            created,
            linked_existing,
            skipped,
        })
    }
}

fn normalize_candidate_url(base_url: &str, candidate: &str) -> Option<String> {
    let candidate = candidate.trim();
    if candidate.is_empty() {
        return None;
    }

    let base = Url::parse(base_url).ok()?;
    let mut resolved = base.join(candidate).ok()?;
    match resolved.scheme() {
        "http" | "https" => {}
        _ => return None,
    }

    resolved.set_fragment(None);
    Some(resolved.to_string())
}

fn extract_markdown_urls(markdown: &str) -> Vec<String> {
    let mut links = extract_bracket_links(markdown);
    links.extend(extract_autolinks(markdown));
    links
}

fn extract_bracket_links(markdown: &str) -> Vec<String> {
    let bytes = markdown.as_bytes();
    let mut links = Vec::new();
    let mut idx = 0usize;

    while idx < bytes.len() {
        if bytes[idx] != b'[' {
            idx += 1;
            continue;
        }

        let Some(label_end) = find_byte(bytes, idx + 1, b']') else {
            break;
        };
        if label_end + 1 >= bytes.len() || bytes[label_end + 1] != b'(' {
            idx = label_end + 1;
            continue;
        }

        let Some(dest_end) = find_link_destination_end(bytes, label_end + 2) else {
            idx = label_end + 1;
            continue;
        };

        let destination_raw = &markdown[label_end + 2..dest_end];
        if let Some(destination) = parse_link_destination(destination_raw) {
            links.push(destination.to_string());
        }
        idx = dest_end + 1;
    }

    links
}

fn extract_autolinks(markdown: &str) -> Vec<String> {
    let bytes = markdown.as_bytes();
    let mut links = Vec::new();
    let mut idx = 0usize;
    while idx < bytes.len() {
        if bytes[idx] != b'<' {
            idx += 1;
            continue;
        }

        let Some(end) = find_byte(bytes, idx + 1, b'>') else {
            break;
        };
        let inner = markdown[idx + 1..end].trim();
        if inner.starts_with("http://") || inner.starts_with("https://") {
            links.push(inner.to_string());
        }
        idx = end + 1;
    }
    links
}

fn find_byte(bytes: &[u8], start: usize, needle: u8) -> Option<usize> {
    bytes
        .iter()
        .enumerate()
        .skip(start)
        .find_map(|(idx, b)| if *b == needle { Some(idx) } else { None })
}

fn find_link_destination_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut idx = start;
    while idx < bytes.len() {
        let byte = bytes[idx];
        if byte == b'\\' && idx + 1 < bytes.len() {
            idx += 2;
            continue;
        }
        if byte == b'(' {
            depth += 1;
        } else if byte == b')' {
            if depth == 0 {
                return Some(idx);
            }
            depth -= 1;
        }
        idx += 1;
    }
    None
}

fn parse_link_destination(destination: &str) -> Option<&str> {
    let destination = destination.trim();
    if destination.is_empty() {
        return None;
    }

    if destination.starts_with('<') {
        let close = destination.find('>')?;
        let inner = destination[1..close].trim();
        if inner.is_empty() { None } else { Some(inner) }
    } else {
        destination
            .split_whitespace()
            .next()
            .filter(|value| !value.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_inline_and_autolink_urls() {
        let markdown = r#"
        [Example](https://example.com/a)
        [Relative](/docs/start)
        <https://example.net/x>
        "#;

        let urls = extract_markdown_urls(markdown);
        assert!(urls.contains(&"https://example.com/a".to_string()));
        assert!(urls.contains(&"/docs/start".to_string()));
        assert!(urls.contains(&"https://example.net/x".to_string()));
    }

    #[test]
    fn parses_link_destinations_with_title() {
        let destination = r#"https://example.com "Title""#;
        assert_eq!(
            parse_link_destination(destination),
            Some("https://example.com")
        );
    }

    #[test]
    fn normalizes_relative_urls_against_base() {
        let normalized = normalize_candidate_url("https://example.com/posts/1", "../about")
            .expect("relative url should resolve");
        assert_eq!(normalized, "https://example.com/about");
    }

    #[test]
    fn rejects_non_http_schemes() {
        assert!(normalize_candidate_url("https://example.com", "mailto:hi@example.com").is_none());
        assert!(normalize_candidate_url("https://example.com", "javascript:alert(1)").is_none());
    }

    #[test]
    fn strips_fragments() {
        let normalized = normalize_candidate_url("https://example.com", "/guide#intro")
            .expect("url should normalize");
        assert_eq!(normalized, "https://example.com/guide");
    }
}
