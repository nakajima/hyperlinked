use super::*;
use crate::{entity::hyperlink_processing_job, test_support};
use sea_orm::EntityTrait;

async fn new_connection() -> DatabaseConnection {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_schema(&connection).await;
    test_support::execute_sql(
        &connection,
        r#"
            INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, source_type, created_at, updated_at)
            VALUES (1, 'Example', 'https://example.com', 'https://example.com', 0, NULL, 'unknown', '2026-03-01 00:00:00', '2026-03-01 00:00:00');
        "#,
    )
    .await;
    connection
}

#[tokio::test]
async fn og_request_queues_snapshot_dependency_when_source_missing() {
    let connection = new_connection().await;

    let result = resolve_and_enqueue_for_job_kind(
        &connection,
        1,
        HyperlinkProcessingJobKind::Og,
        ArtifactFetchMode::EnsurePresent,
        None,
    )
    .await
    .expect("dependency resolution should succeed");

    assert_eq!(
        result,
        ArtifactJobResolveResult::EnqueuedDependency {
            requested_kind: HyperlinkProcessingJobKind::Og,
            dependency_kind: HyperlinkProcessingJobKind::Snapshot,
            queued_job_id: 1,
        }
    );
}

#[tokio::test]
async fn og_request_queues_requested_job_when_source_exists() {
    let connection = new_connection().await;
    test_support::execute_sql(
        &connection,
        r#"
            INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
            VALUES (1, 1, NULL, 'snapshot_warc', X'77617263', 'application/warc', 4, '2026-03-01 00:00:10');
        "#,
    )
    .await;

    let result = resolve_and_enqueue_for_job_kind(
        &connection,
        1,
        HyperlinkProcessingJobKind::Og,
        ArtifactFetchMode::EnsurePresent,
        None,
    )
    .await
    .expect("dependency resolution should succeed");

    assert_eq!(
        result,
        ArtifactJobResolveResult::EnqueuedRequested {
            requested_kind: HyperlinkProcessingJobKind::Og,
            queued_job_id: 1,
        }
    );
}

#[tokio::test]
async fn ensure_mode_skips_readability_when_already_satisfied() {
    let connection = new_connection().await;
    test_support::execute_sql(
        &connection,
        r#"
            INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
            VALUES
                (1, 1, NULL, 'snapshot_warc', X'77617263', 'application/warc', 4, '2026-03-01 00:00:10'),
                (2, 1, NULL, 'readable_text', X'74657874', 'text/markdown; charset=utf-8', 4, '2026-03-01 00:00:11'),
                (3, 1, NULL, 'readable_meta', X'7B7D', 'application/json', 2, '2026-03-01 00:00:12');
        "#,
    )
    .await;

    let result = resolve_and_enqueue_for_job_kind(
        &connection,
        1,
        HyperlinkProcessingJobKind::Readability,
        ArtifactFetchMode::EnsurePresent,
        None,
    )
    .await
    .expect("dependency resolution should succeed");

    assert_eq!(
        result,
        ArtifactJobResolveResult::AlreadySatisfied {
            requested_kind: HyperlinkProcessingJobKind::Readability,
        }
    );

    let jobs = hyperlink_processing_job::Entity::find()
        .all(&connection)
        .await
        .expect("jobs should load");
    assert!(jobs.is_empty());
}

#[tokio::test]
async fn refetch_mode_queues_even_when_requested_condition_is_satisfied() {
    let connection = new_connection().await;
    test_support::execute_sql(
        &connection,
        r#"
            INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
            VALUES
                (1, 1, NULL, 'snapshot_warc', X'77617263', 'application/warc', 4, '2026-03-01 00:00:10'),
                (2, 1, NULL, 'og_meta', X'7B7D', 'application/json', 2, '2026-03-01 00:00:11');
        "#,
    )
    .await;

    let result = resolve_and_enqueue_for_job_kind(
        &connection,
        1,
        HyperlinkProcessingJobKind::Og,
        ArtifactFetchMode::RefetchTarget,
        None,
    )
    .await
    .expect("dependency resolution should succeed");

    assert_eq!(
        result,
        ArtifactJobResolveResult::EnqueuedRequested {
            requested_kind: HyperlinkProcessingJobKind::Og,
            queued_job_id: 1,
        }
    );
}

#[tokio::test]
async fn disabled_requested_kind_is_blocked() {
    let connection = new_connection().await;
    settings::save(
        &connection,
        ArtifactCollectionSettings {
            collect_source: true,
            collect_screenshots: true,
            collect_screenshot_dark: true,
            collect_og: false,
            collect_readability: true,
        },
    )
    .await
    .expect("settings should save");

    let result = resolve_and_enqueue_for_job_kind(
        &connection,
        1,
        HyperlinkProcessingJobKind::Og,
        ArtifactFetchMode::EnsurePresent,
        None,
    )
    .await
    .expect("dependency resolution should succeed");

    assert_eq!(
        result,
        ArtifactJobResolveResult::DisabledRequested {
            requested_kind: HyperlinkProcessingJobKind::Og,
        }
    );
}

#[tokio::test]
async fn disabled_dependency_is_reported() {
    let connection = new_connection().await;

    let result = resolve_and_enqueue_for_job_kind_with_settings(
        &connection,
        1,
        HyperlinkProcessingJobKind::Og,
        ArtifactFetchMode::EnsurePresent,
        ArtifactCollectionSettings {
            collect_source: false,
            collect_screenshots: false,
            collect_screenshot_dark: false,
            collect_og: true,
            collect_readability: true,
        },
        None,
    )
    .await
    .expect("dependency resolution should succeed");

    assert_eq!(
        result,
        ArtifactJobResolveResult::DisabledDependency {
            requested_kind: HyperlinkProcessingJobKind::Og,
            dependency_kind: HyperlinkProcessingJobKind::Snapshot,
        }
    );
}

#[tokio::test]
async fn unfetchable_dependency_is_reported_for_relative_pdf_url() {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_schema(&connection).await;
    test_support::execute_sql(
        &connection,
        r#"
            INSERT INTO hyperlink (id, title, url, raw_url, clicks_count, last_clicked_at, source_type, created_at, updated_at)
            VALUES (1, 'Upload', '/uploads/1/paper.pdf', '/uploads/1/paper.pdf', 0, NULL, 'pdf', '2026-03-01 00:00:00', '2026-03-01 00:00:00');
        "#,
    )
    .await;

    let result = resolve_and_enqueue_for_job_kind(
        &connection,
        1,
        HyperlinkProcessingJobKind::Readability,
        ArtifactFetchMode::EnsurePresent,
        None,
    )
    .await
    .expect("dependency resolution should succeed");

    assert_eq!(
        result,
        ArtifactJobResolveResult::UnfetchableDependency {
            requested_kind: HyperlinkProcessingJobKind::Readability,
            dependency_kind: HyperlinkProcessingJobKind::Snapshot,
        }
    );
}

#[test]
fn artifact_kind_enablement_uses_job_spec_settings() {
    let settings = ArtifactCollectionSettings {
        collect_source: true,
        collect_screenshots: false,
        collect_screenshot_dark: false,
        collect_og: false,
        collect_readability: true,
    };

    assert!(!artifact_kind_fetch_enabled(
        &HyperlinkArtifactKind::OgMeta,
        settings
    ));
    assert!(artifact_kind_fetch_enabled(
        &HyperlinkArtifactKind::ReadableText,
        settings
    ));
    assert!(!artifact_kind_fetch_enabled(
        &HyperlinkArtifactKind::PaperlessMetadata,
        settings
    ));
}
