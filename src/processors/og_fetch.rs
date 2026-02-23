use std::collections::HashMap;

use sea_orm::ActiveValue::Set;
use sea_orm::DatabaseConnection;
use serde::Serialize;

use crate::{
    entity::{
        hyperlink,
        hyperlink_artifact::{self as hyperlink_artifact_entity, HyperlinkArtifactKind},
    },
    model::hyperlink_artifact as hyperlink_artifact_model,
    processors::{
        processor::{ProcessingError, Processor},
        readability_fetch::extract_html_from_warc,
    },
};

const OG_META_CONTENT_TYPE: &str = "application/json";
const OG_ERROR_CONTENT_TYPE: &str = "application/json";

pub struct OgFetcher {
    job_id: i32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OgFetchOutput {
    pub meta_artifact_id: Option<i32>,
    pub error_artifact_id: Option<i32>,
}

impl OgFetcher {
    pub fn new(job_id: i32) -> Self {
        Self { job_id }
    }
}

impl Processor for OgFetcher {
    type Output = OgFetchOutput;

    async fn process<'a>(
        &'a mut self,
        hyperlink: &'a mut hyperlink::ActiveModel,
        connection: &'a DatabaseConnection,
    ) -> Result<Self::Output, ProcessingError> {
        let hyperlink_id = *hyperlink.id.as_ref();
        let source_url = hyperlink.url.as_ref().to_string();

        let snapshot_artifact = hyperlink_artifact_model::latest_for_hyperlink_kind(
            connection,
            hyperlink_id,
            HyperlinkArtifactKind::SnapshotWarc,
        )
        .await
        .map_err(ProcessingError::DB)?;

        if snapshot_artifact.is_none() {
            if let Some(pdf_artifact) = hyperlink_artifact_model::latest_for_hyperlink_kind(
                connection,
                hyperlink_id,
                HyperlinkArtifactKind::PdfSource,
            )
            .await
            .map_err(ProcessingError::DB)?
            {
                apply_selected_fields(hyperlink, &OgSelected::default());

                let payload = serde_json::to_vec_pretty(&OgMetaArtifact {
                    source_url: source_url.clone(),
                    captured_at: now_utc().to_string(),
                    source_kind: "pdf".to_string(),
                    source_artifact_id: Some(pdf_artifact.id),
                    selected: OgSelected::default(),
                    tags: Vec::new(),
                })
                .unwrap_or_else(|encode_error| {
                    format!("{{\"error\":\"failed to encode og_meta payload: {encode_error}\"}}")
                        .into_bytes()
                });

                let meta_artifact = hyperlink_artifact_model::insert(
                    connection,
                    hyperlink_id,
                    Some(self.job_id),
                    HyperlinkArtifactKind::OgMeta,
                    payload,
                    OG_META_CONTENT_TYPE,
                )
                .await
                .map_err(ProcessingError::DB)?;

                return Ok(OgFetchOutput {
                    meta_artifact_id: Some(meta_artifact.id),
                    error_artifact_id: None,
                });
            }

            let error_artifact = persist_og_error(
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
                "og extraction requires snapshot_warc or pdf_source artifacts (error_artifact_id={})",
                error_artifact.id
            )));
        }

        let snapshot_artifact = snapshot_artifact.expect("already guarded");
        let snapshot_payload = hyperlink_artifact_model::load_payload(&snapshot_artifact)
            .await
            .map_err(ProcessingError::DB)?;
        let html = match extract_html_from_warc(&snapshot_payload) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
            Err(error) => {
                let error_artifact = persist_og_error(
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

        let tags = extract_open_graph_tags(&html);
        let selected = select_open_graph_fields(&tags);
        apply_selected_fields(hyperlink, &selected);

        let payload = serde_json::to_vec_pretty(&OgMetaArtifact {
            source_url: source_url.clone(),
            captured_at: now_utc().to_string(),
            source_kind: "html".to_string(),
            source_artifact_id: Some(snapshot_artifact.id),
            selected,
            tags,
        })
        .map_err(|error| {
            ProcessingError::FetchError(format!("failed to encode og metadata payload: {error}"))
        })?;

        let meta_artifact = hyperlink_artifact_model::insert(
            connection,
            hyperlink_id,
            Some(self.job_id),
            HyperlinkArtifactKind::OgMeta,
            payload,
            OG_META_CONTENT_TYPE,
        )
        .await
        .map_err(ProcessingError::DB)?;

        Ok(OgFetchOutput {
            meta_artifact_id: Some(meta_artifact.id),
            error_artifact_id: None,
        })
    }
}

#[derive(Clone, Debug, Serialize)]
struct OgMetaArtifact {
    source_url: String,
    captured_at: String,
    source_kind: String,
    source_artifact_id: Option<i32>,
    selected: OgSelected,
    tags: Vec<OgTag>,
}

#[derive(Clone, Debug, Default, Serialize)]
struct OgSelected {
    title: Option<String>,
    description: Option<String>,
    og_type: Option<String>,
    url: Option<String>,
    image: Option<String>,
    site_name: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct OgTag {
    property: String,
    content: String,
}

#[derive(Serialize)]
struct OgErrorArtifact {
    source_url: String,
    stage: String,
    error: String,
    failed_at: String,
}

fn extract_open_graph_tags(html: &str) -> Vec<OgTag> {
    let mut tags = Vec::new();
    let mut cursor = 0usize;
    let lowercase = html.to_ascii_lowercase();
    let bytes = html.as_bytes();

    while let Some(meta_idx) = lowercase[cursor..].find("<meta") {
        let tag_start = cursor + meta_idx;
        let Some(tag_end) = find_tag_end(bytes, tag_start + "<meta".len()) else {
            break;
        };
        if tag_end <= tag_start + "<meta".len() {
            cursor = tag_start + "<meta".len();
            continue;
        }

        let attrs = parse_html_attributes(&html[tag_start + "<meta".len()..tag_end]);
        let property = attrs
            .get("property")
            .or_else(|| attrs.get("name"))
            .map(String::as_str)
            .unwrap_or_default();
        let property = normalize_property(property);

        if !property.starts_with("og:") {
            cursor = tag_end + 1;
            continue;
        }

        let content = attrs.get("content").map(String::as_str).unwrap_or_default();
        let content = collapse_whitespace(content.trim());
        if content.is_empty() {
            cursor = tag_end + 1;
            continue;
        }

        tags.push(OgTag { property, content });
        cursor = tag_end + 1;
    }

    tags
}

fn select_open_graph_fields(tags: &[OgTag]) -> OgSelected {
    let mut first_by_property = HashMap::<&str, &str>::new();
    for tag in tags {
        first_by_property
            .entry(tag.property.as_str())
            .or_insert(tag.content.as_str());
    }

    OgSelected {
        title: first_by_property.get("og:title").map(ToString::to_string),
        description: first_by_property
            .get("og:description")
            .map(ToString::to_string),
        og_type: first_by_property.get("og:type").map(ToString::to_string),
        url: first_by_property.get("og:url").map(ToString::to_string),
        image: first_by_property.get("og:image").map(ToString::to_string),
        site_name: first_by_property
            .get("og:site_name")
            .map(ToString::to_string),
    }
}

fn apply_selected_fields(hyperlink: &mut hyperlink::ActiveModel, selected: &OgSelected) {
    hyperlink.og_title = Set(selected.title.clone());
    hyperlink.og_description = Set(selected.description.clone());
    hyperlink.og_type = Set(selected.og_type.clone());
    hyperlink.og_url = Set(selected.url.clone());
    hyperlink.og_image_url = Set(selected.image.clone());
    hyperlink.og_site_name = Set(selected.site_name.clone());
}

fn normalize_property(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_html_attributes(raw: &str) -> HashMap<String, String> {
    let bytes = raw.as_bytes();
    let mut idx = 0usize;
    let mut attributes = HashMap::new();

    while idx < bytes.len() {
        idx = skip_whitespace_and_slashes(bytes, idx);
        if idx >= bytes.len() {
            break;
        }

        let name_start = idx;
        while idx < bytes.len() && is_attribute_name_char(bytes[idx]) {
            idx += 1;
        }
        if name_start == idx {
            idx += 1;
            continue;
        }

        let name = raw[name_start..idx].trim().to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }

        idx = skip_html_whitespace(bytes, idx);
        let mut value = String::new();

        if idx < bytes.len() && bytes[idx] == b'=' {
            idx += 1;
            idx = skip_html_whitespace(bytes, idx);
            if idx < bytes.len() && (bytes[idx] == b'"' || bytes[idx] == b'\'') {
                let quote = bytes[idx];
                idx += 1;
                let value_start = idx;
                while idx < bytes.len() && bytes[idx] != quote {
                    idx += 1;
                }
                value = raw[value_start..idx].to_string();
                if idx < bytes.len() {
                    idx += 1;
                }
            } else {
                let value_start = idx;
                while idx < bytes.len() && !is_html_whitespace(bytes[idx]) && bytes[idx] != b'>' {
                    idx += 1;
                }
                value = raw[value_start..idx].trim_end_matches('/').to_string();
            }
        }

        attributes.entry(name).or_insert(value);
    }

    attributes
}

fn find_tag_end(bytes: &[u8], mut idx: usize) -> Option<usize> {
    let mut quote = None;
    while idx < bytes.len() {
        let byte = bytes[idx];
        match quote {
            Some(active_quote) if byte == active_quote => quote = None,
            Some(_) => {}
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None if byte == b'>' => return Some(idx),
            None => {}
        }
        idx += 1;
    }
    None
}

fn skip_whitespace_and_slashes(bytes: &[u8], mut idx: usize) -> usize {
    while idx < bytes.len() && (is_html_whitespace(bytes[idx]) || bytes[idx] == b'/') {
        idx += 1;
    }
    idx
}

fn skip_html_whitespace(bytes: &[u8], mut idx: usize) -> usize {
    while idx < bytes.len() && is_html_whitespace(bytes[idx]) {
        idx += 1;
    }
    idx
}

fn is_html_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\n' | b'\r' | b'\t' | 0x0C)
}

fn is_attribute_name_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'-')
}

async fn persist_og_error(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    job_id: i32,
    source_url: &str,
    stage: &str,
    error: &str,
) -> Result<hyperlink_artifact_entity::Model, sea_orm::DbErr> {
    let payload = serde_json::to_vec_pretty(&OgErrorArtifact {
        source_url: source_url.to_string(),
        stage: stage.to_string(),
        error: error.to_string(),
        failed_at: now_utc().to_string(),
    })
    .unwrap_or_else(|encode_error| {
        format!("{{\"error\":\"failed to encode og_error artifact: {encode_error}\"}}").into_bytes()
    });

    hyperlink_artifact_model::insert(
        connection,
        hyperlink_id,
        Some(job_id),
        HyperlinkArtifactKind::OgError,
        payload,
        OG_ERROR_CONTENT_TYPE,
    )
    .await
}

fn now_utc() -> sea_orm::entity::prelude::DateTime {
    sea_orm::entity::prelude::DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        entity::{hyperlink, hyperlink_artifact},
        model::hyperlink_artifact as hyperlink_artifact_model,
        processors::processor::Processor,
        server::test_support,
    };
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait};
    use serde_json::json;

    #[test]
    fn extracts_open_graph_tags_from_meta_elements() {
        let html = r#"
            <meta property="og:title" content="Example title">
            <meta property="og:description" content="  Example   description  ">
            <meta property="og:type" content="article">
            <meta property="og:image" content="https://cdn.example.com/image.jpg">
            <meta property="twitter:title" content="Not OG">
        "#;

        let tags = extract_open_graph_tags(html);
        assert_eq!(tags.len(), 4);

        let selected = select_open_graph_fields(&tags);
        assert_eq!(selected.title.as_deref(), Some("Example title"));
        assert_eq!(selected.description.as_deref(), Some("Example description"));
        assert_eq!(selected.og_type.as_deref(), Some("article"));
        assert_eq!(
            selected.image.as_deref(),
            Some("https://cdn.example.com/image.jpg")
        );
    }

    #[test]
    fn accepts_name_attribute_for_og_properties() {
        let html = r#"<meta name="og:title" content="Name Attribute">"#;
        let tags = extract_open_graph_tags(html);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].property, "og:title");
        assert_eq!(tags[0].content, "Name Attribute");
    }

    #[test]
    fn parses_unquoted_meta_attributes() {
        let html = r#"<meta property=og:url content=https://example.com/path>"#;
        let tags = extract_open_graph_tags(html);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].property, "og:url");
        assert_eq!(tags[0].content, "https://example.com/path");
    }

    #[tokio::test]
    async fn process_sets_hyperlink_og_fields_and_persists_meta_artifact() {
        let connection = test_support::new_memory_connection().await;
        test_support::initialize_hyperlinks_schema_with_search(&connection).await;

        test_support::execute_sql(
            &connection,
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example', 'https://example.com/post', 'https://example.com/post', 0, 0, NULL, '2026-02-22 00:00:00', '2026-02-22 00:00:00');
            "#,
        )
        .await;

        let html = r#"
            <html><head>
              <meta property="og:title" content="Example OG Title">
              <meta property="og:description" content="Example OG Description">
              <meta property="og:type" content="article">
              <meta property="og:url" content="https://example.com/post">
              <meta property="og:image" content="https://cdn.example.com/post.png">
              <meta property="og:site_name" content="Example Site">
            </head><body></body></html>
        "#;
        let warc_payload = format!(
            "WARC/1.0\r\nWARC-Type: response\r\nWARC-Target-URI: https://example.com/post\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
            html.len(),
            html
        )
        .into_bytes();

        hyperlink_artifact::ActiveModel {
            hyperlink_id: Set(1),
            job_id: Set(None),
            kind: Set(HyperlinkArtifactKind::SnapshotWarc),
            payload: Set(warc_payload.clone()),
            storage_path: Set(None),
            storage_backend: Set(None),
            checksum_sha256: Set(None),
            content_type: Set("application/warc".to_string()),
            size_bytes: Set(i32::try_from(warc_payload.len()).expect("payload len fits in i32")),
            created_at: Set(now_utc()),
            ..Default::default()
        }
        .insert(&connection)
        .await
        .expect("snapshot artifact should insert");

        let mut hyperlink_active: hyperlink::ActiveModel = hyperlink::Entity::find_by_id(1)
            .one(&connection)
            .await
            .expect("query should succeed")
            .expect("row should exist")
            .into();

        let mut fetcher = OgFetcher::new(42);
        let output = fetcher
            .process(&mut hyperlink_active, &connection)
            .await
            .expect("og fetch should succeed");
        assert!(output.meta_artifact_id.is_some());
        assert!(output.error_artifact_id.is_none());

        let updated = hyperlink_active
            .update(&connection)
            .await
            .expect("hyperlink should update");
        assert_eq!(updated.og_title.as_deref(), Some("Example OG Title"));
        assert_eq!(
            updated.og_description.as_deref(),
            Some("Example OG Description")
        );
        assert_eq!(updated.og_type.as_deref(), Some("article"));
        assert_eq!(updated.og_url.as_deref(), Some("https://example.com/post"));
        assert_eq!(
            updated.og_image_url.as_deref(),
            Some("https://cdn.example.com/post.png")
        );
        assert_eq!(updated.og_site_name.as_deref(), Some("Example Site"));

        let meta_artifact = hyperlink_artifact_model::latest_for_hyperlink_kind(
            &connection,
            1,
            HyperlinkArtifactKind::OgMeta,
        )
        .await
        .expect("meta query should succeed")
        .expect("meta artifact should exist");
        let meta_payload = hyperlink_artifact_model::load_payload(&meta_artifact)
            .await
            .expect("meta payload should load");
        let meta_json: serde_json::Value =
            serde_json::from_slice(&meta_payload).expect("payload should be json");
        assert_eq!(meta_json["selected"]["title"], json!("Example OG Title"));
        assert_eq!(
            meta_json["selected"]["description"],
            json!("Example OG Description")
        );
    }
}
