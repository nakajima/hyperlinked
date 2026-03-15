use super::*;
use crate::{
    app::models::hyperlink_artifact as hyperlink_artifact_model,
    entity::{hyperlink, hyperlink_artifact},
    processors::processor::Processor,
    test_support,
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

#[test]
fn resolves_relative_og_image_url_against_source_url() {
    let resolved = resolve_og_image_url("https://example.com/posts/42", "/media/cover.png")
        .expect("relative og:image URL should resolve");
    assert_eq!(resolved.as_str(), "https://example.com/media/cover.png");
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
                INSERT INTO hyperlink_processing_job (id, hyperlink_id, kind, state, error_message, queued_at, started_at, finished_at, created_at, updated_at)
                VALUES (42, 1, 'og', 'running', NULL, '2026-02-22 00:00:01', '2026-02-22 00:00:02', NULL, '2026-02-22 00:00:01', '2026-02-22 00:00:02');
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
    if let Some(image_artifact_id) = output.image_artifact_id {
        let image_artifact = hyperlink_artifact::Entity::find_by_id(image_artifact_id)
            .one(&connection)
            .await
            .expect("image artifact query should succeed")
            .expect("image artifact should exist");
        assert_eq!(image_artifact.kind, HyperlinkArtifactKind::OgImage);
    }

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

#[tokio::test]
async fn process_ignores_non_http_og_image_urls_and_still_persists_meta_artifact() {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_schema_with_search(&connection).await;

    test_support::execute_sql(
            &connection,
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example', 'https://example.com/post', 'https://example.com/post', 0, 0, NULL, '2026-02-22 00:00:00', '2026-02-22 00:00:00');
                INSERT INTO hyperlink_processing_job (id, hyperlink_id, kind, state, error_message, queued_at, started_at, finished_at, created_at, updated_at)
                VALUES (99, 1, 'og', 'running', NULL, '2026-02-22 00:00:01', '2026-02-22 00:00:02', NULL, '2026-02-22 00:00:01', '2026-02-22 00:00:02');
            "#,
        )
        .await;

    let html = r#"
            <html><head>
              <meta property="og:title" content="Example OG Title">
              <meta property="og:image" content="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB">
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

    let mut fetcher = OgFetcher::new(99);
    let output = fetcher
        .process(&mut hyperlink_active, &connection)
        .await
        .expect("og fetch should succeed");
    assert!(output.meta_artifact_id.is_some());
    assert!(output.image_artifact_id.is_none());
    assert!(output.error_artifact_id.is_none());

    let image_artifact = hyperlink_artifact_model::latest_for_hyperlink_kind(
        &connection,
        1,
        HyperlinkArtifactKind::OgImage,
    )
    .await
    .expect("image artifact query should succeed");
    assert!(image_artifact.is_none());
}
