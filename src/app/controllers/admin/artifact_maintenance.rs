use std::collections::{HashMap, HashSet};

use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, QuerySelect,
    Statement,
};

use crate::{
    app::models::{
        artifact_job::{self, ArtifactFetchMode},
        hyperlink_processing_job::{self as hyperlink_processing_job_model, ProcessingQueueSender},
        settings::{self, ArtifactCollectionSettings},
    },
    entity::{
        hyperlink,
        hyperlink_artifact::{self, HyperlinkArtifactKind},
        hyperlink_processing_job::{self, HyperlinkProcessingJobKind, HyperlinkProcessingJobState},
        hyperlink_search_doc,
    },
};

#[derive(Clone, Copy, Debug)]
pub(super) struct LastRunSummary {
    pub(super) snapshot_queued: usize,
    pub(super) og_queued: usize,
    pub(super) readability_queued: usize,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct MissingArtifactsSummary {
    pub(super) total_hyperlinks: usize,
    pub(super) missing_source: usize,
    pub(super) missing_og: usize,
    pub(super) missing_readability: usize,
    pub(super) snapshot_already_processing: usize,
    pub(super) og_already_processing: usize,
    pub(super) readability_already_processing: usize,
    pub(super) snapshot_will_queue: usize,
    pub(super) og_will_queue: usize,
    pub(super) readability_will_queue: usize,
}

#[derive(Default)]
struct ArtifactPresence {
    has_source: bool,
    has_screenshot: bool,
    has_og_meta: bool,
    has_readable_text: bool,
    has_readable_meta: bool,
}

pub(super) struct MissingArtifactsPlan {
    pub(super) summary: MissingArtifactsSummary,
    pub(super) snapshot_hyperlink_ids: Vec<i32>,
    pub(super) screenshot_hyperlink_ids: Vec<i32>,
    pub(super) og_hyperlink_ids: Vec<i32>,
    pub(super) readability_hyperlink_ids: Vec<i32>,
}

pub(super) fn checkbox_checked(value: &Option<String>) -> bool {
    value.is_some()
}

pub(super) fn build_artifact_settings_message(
    deleted_source: u64,
    deleted_screenshots: u64,
    deleted_og: u64,
    deleted_readability: u64,
    queued_snapshot: usize,
    queued_screenshots: usize,
    queued_og: usize,
    queued_readability: usize,
    queue_warning: Option<&str>,
) -> String {
    let mut parts = vec!["Updated artifact settings.".to_string()];

    if deleted_source > 0 || deleted_screenshots > 0 || deleted_og > 0 || deleted_readability > 0 {
        parts.push(format!(
            "Deleted source={deleted_source}, screenshots={deleted_screenshots}, og={deleted_og}, readability={deleted_readability}."
        ));
    }

    if queued_snapshot > 0 || queued_screenshots > 0 || queued_og > 0 || queued_readability > 0 {
        parts.push(format!(
            "Queued backfill snapshot={queued_snapshot}, screenshots={queued_screenshots}, og={queued_og}, readability={queued_readability}."
        ));
    }

    if let Some(warning) = queue_warning {
        parts.push(warning.to_string());
    }

    parts.join(" ")
}

pub(super) async fn enqueue_hyperlink_jobs(
    connection: &DatabaseConnection,
    queue: Option<&ProcessingQueueSender>,
    kind: HyperlinkProcessingJobKind,
    hyperlink_ids: &[i32],
) -> Result<usize, DbErr> {
    let artifact_settings = settings::load(connection).await?;
    let mut queued = 0usize;
    for hyperlink_id in hyperlink_ids {
        if matches!(
            kind,
            HyperlinkProcessingJobKind::Snapshot
                | HyperlinkProcessingJobKind::Og
                | HyperlinkProcessingJobKind::Readability
        ) {
            let result = artifact_job::resolve_and_enqueue_for_job_kind_with_settings(
                connection,
                *hyperlink_id,
                kind.clone(),
                ArtifactFetchMode::EnsurePresent,
                artifact_settings,
                queue,
            )
            .await?;
            if result.was_enqueued() {
                queued += 1;
            }
            continue;
        }

        hyperlink_processing_job_model::enqueue_for_hyperlink_kind(
            connection,
            *hyperlink_id,
            kind.clone(),
            queue,
        )
        .await?;
        queued += 1;
    }
    Ok(queued)
}

pub(super) async fn delete_source_artifacts(connection: &DatabaseConnection) -> Result<u64, DbErr> {
    delete_artifacts_for_kinds(
        connection,
        &[
            HyperlinkArtifactKind::SnapshotWarc,
            HyperlinkArtifactKind::PdfSource,
            HyperlinkArtifactKind::SnapshotError,
        ],
    )
    .await
}

pub(super) async fn delete_screenshot_artifacts(
    connection: &DatabaseConnection,
) -> Result<u64, DbErr> {
    delete_artifacts_for_kinds(
        connection,
        &[
            HyperlinkArtifactKind::ScreenshotWebp,
            HyperlinkArtifactKind::ScreenshotThumbWebp,
            HyperlinkArtifactKind::ScreenshotDarkWebp,
            HyperlinkArtifactKind::ScreenshotThumbDarkWebp,
            HyperlinkArtifactKind::ScreenshotError,
        ],
    )
    .await
}

pub(super) async fn delete_og_artifacts_and_clear_fields(
    connection: &DatabaseConnection,
) -> Result<u64, DbErr> {
    let deleted = delete_artifacts_for_kinds(
        connection,
        &[
            HyperlinkArtifactKind::OgMeta,
            HyperlinkArtifactKind::OgImage,
            HyperlinkArtifactKind::OgError,
        ],
    )
    .await?;
    clear_hyperlink_og_fields(connection).await?;
    Ok(deleted)
}

pub(super) async fn delete_readability_artifacts_and_clear_search(
    connection: &DatabaseConnection,
) -> Result<u64, DbErr> {
    let deleted = delete_artifacts_for_kinds(
        connection,
        &[
            HyperlinkArtifactKind::ReadableText,
            HyperlinkArtifactKind::ReadableHtml,
            HyperlinkArtifactKind::ReadableMeta,
            HyperlinkArtifactKind::ReadableError,
        ],
    )
    .await?;

    match hyperlink_search_doc::clear_all_readable_text(connection).await {
        Ok(_) => {}
        Err(error) if hyperlink_search_doc::is_missing_table_error(&error) => {}
        Err(error) => return Err(error),
    }

    Ok(deleted)
}

async fn delete_artifacts_for_kinds(
    connection: &DatabaseConnection,
    kinds: &[HyperlinkArtifactKind],
) -> Result<u64, DbErr> {
    if kinds.is_empty() {
        return Ok(0);
    }

    let result = hyperlink_artifact::Entity::delete_many()
        .filter(hyperlink_artifact::Column::Kind.is_in(kinds.to_vec()))
        .exec(connection)
        .await?;
    Ok(result.rows_affected)
}

async fn clear_hyperlink_og_fields(connection: &DatabaseConnection) -> Result<u64, DbErr> {
    let backend = connection.get_database_backend();
    let result = connection
        .execute_raw(Statement::from_string(
            backend,
            r#"
                UPDATE hyperlink
                SET
                    og_title = NULL,
                    og_description = NULL,
                    og_type = NULL,
                    og_url = NULL,
                    og_image_url = NULL,
                    og_site_name = NULL,
                    updated_at = CURRENT_TIMESTAMP
                WHERE
                    og_title IS NOT NULL
                    OR og_description IS NOT NULL
                    OR og_type IS NOT NULL
                    OR og_url IS NOT NULL
                    OR og_image_url IS NOT NULL
                    OR og_site_name IS NOT NULL
            "#
            .to_string(),
        ))
        .await?;
    Ok(result.rows_affected())
}

pub(super) async fn build_missing_artifacts_plan(
    connection: &DatabaseConnection,
    artifact_settings: &ArtifactCollectionSettings,
) -> Result<MissingArtifactsPlan, DbErr> {
    let hyperlink_ids = hyperlink::Entity::find()
        .select_only()
        .column(hyperlink::Column::Id)
        .into_tuple::<i32>()
        .all(connection)
        .await?;

    let mut summary = MissingArtifactsSummary {
        total_hyperlinks: hyperlink_ids.len(),
        ..Default::default()
    };

    if hyperlink_ids.is_empty() {
        return Ok(MissingArtifactsPlan {
            summary,
            snapshot_hyperlink_ids: Vec::new(),
            screenshot_hyperlink_ids: Vec::new(),
            og_hyperlink_ids: Vec::new(),
            readability_hyperlink_ids: Vec::new(),
        });
    }

    let artifact_rows = hyperlink_artifact::Entity::find()
        .select_only()
        .column(hyperlink_artifact::Column::HyperlinkId)
        .column(hyperlink_artifact::Column::Kind)
        .filter(hyperlink_artifact::Column::HyperlinkId.is_in(hyperlink_ids.clone()))
        .filter(hyperlink_artifact::Column::Kind.is_in([
            HyperlinkArtifactKind::SnapshotWarc,
            HyperlinkArtifactKind::PdfSource,
            HyperlinkArtifactKind::ScreenshotWebp,
            HyperlinkArtifactKind::ScreenshotThumbWebp,
            HyperlinkArtifactKind::ScreenshotDarkWebp,
            HyperlinkArtifactKind::ScreenshotThumbDarkWebp,
            HyperlinkArtifactKind::OgMeta,
            HyperlinkArtifactKind::ReadableText,
            HyperlinkArtifactKind::ReadableMeta,
        ]))
        .into_tuple::<(i32, HyperlinkArtifactKind)>()
        .all(connection)
        .await?;

    let mut artifact_presence_by_hyperlink = HashMap::<i32, ArtifactPresence>::new();
    for (hyperlink_id, kind) in artifact_rows {
        let presence = artifact_presence_by_hyperlink
            .entry(hyperlink_id)
            .or_default();
        match kind {
            HyperlinkArtifactKind::SnapshotWarc | HyperlinkArtifactKind::PdfSource => {
                presence.has_source = true;
            }
            HyperlinkArtifactKind::ScreenshotWebp
            | HyperlinkArtifactKind::ScreenshotThumbWebp
            | HyperlinkArtifactKind::ScreenshotDarkWebp
            | HyperlinkArtifactKind::ScreenshotThumbDarkWebp => presence.has_screenshot = true,
            HyperlinkArtifactKind::OgMeta => {
                presence.has_og_meta = true;
            }
            HyperlinkArtifactKind::ReadableText => {
                presence.has_readable_text = true;
            }
            HyperlinkArtifactKind::ReadableMeta => {
                presence.has_readable_meta = true;
            }
            _ => {}
        }
    }

    let active_rows = hyperlink_processing_job::Entity::find()
        .select_only()
        .column(hyperlink_processing_job::Column::HyperlinkId)
        .column(hyperlink_processing_job::Column::Kind)
        .filter(hyperlink_processing_job::Column::HyperlinkId.is_in(hyperlink_ids.clone()))
        .filter(hyperlink_processing_job::Column::State.is_in([
            HyperlinkProcessingJobState::Queued,
            HyperlinkProcessingJobState::Running,
        ]))
        .filter(hyperlink_processing_job::Column::Kind.is_in([
            HyperlinkProcessingJobKind::Snapshot,
            HyperlinkProcessingJobKind::Og,
            HyperlinkProcessingJobKind::Readability,
        ]))
        .into_tuple::<(i32, HyperlinkProcessingJobKind)>()
        .all(connection)
        .await?;

    let mut snapshot_active_hyperlinks = HashSet::<i32>::new();
    let mut og_active_hyperlinks = HashSet::<i32>::new();
    let mut readability_active_hyperlinks = HashSet::<i32>::new();
    for (hyperlink_id, kind) in active_rows {
        match kind {
            HyperlinkProcessingJobKind::Snapshot => {
                snapshot_active_hyperlinks.insert(hyperlink_id);
            }
            HyperlinkProcessingJobKind::Og => {
                og_active_hyperlinks.insert(hyperlink_id);
            }
            HyperlinkProcessingJobKind::Readability => {
                readability_active_hyperlinks.insert(hyperlink_id);
            }
            _ => {}
        }
    }

    let mut snapshot_hyperlink_ids = Vec::new();
    let mut screenshot_hyperlink_ids = Vec::new();
    let mut og_hyperlink_ids = Vec::new();
    let mut readability_hyperlink_ids = Vec::new();

    for hyperlink_id in hyperlink_ids {
        let presence = artifact_presence_by_hyperlink.get(&hyperlink_id);
        let has_source = presence.is_some_and(|presence| presence.has_source);
        if !has_source {
            summary.missing_source += 1;
            if artifact_settings.collect_source {
                if snapshot_active_hyperlinks.contains(&hyperlink_id) {
                    summary.snapshot_already_processing += 1;
                } else {
                    snapshot_hyperlink_ids.push(hyperlink_id);
                }
            }
            continue;
        }

        let has_screenshot = presence.is_some_and(|presence| presence.has_screenshot);
        if artifact_settings.collect_screenshots
            && !has_screenshot
            && !snapshot_active_hyperlinks.contains(&hyperlink_id)
        {
            screenshot_hyperlink_ids.push(hyperlink_id);
        }

        let has_og_meta = presence.is_some_and(|presence| presence.has_og_meta);
        if !has_og_meta {
            summary.missing_og += 1;
            if artifact_settings.collect_og {
                if og_active_hyperlinks.contains(&hyperlink_id) {
                    summary.og_already_processing += 1;
                } else {
                    og_hyperlink_ids.push(hyperlink_id);
                }
            }
        }

        let has_readable_artifacts = presence
            .is_some_and(|presence| presence.has_readable_text && presence.has_readable_meta);
        if !has_readable_artifacts {
            summary.missing_readability += 1;
            if artifact_settings.collect_readability {
                if readability_active_hyperlinks.contains(&hyperlink_id) {
                    summary.readability_already_processing += 1;
                } else {
                    readability_hyperlink_ids.push(hyperlink_id);
                }
            }
        }
    }

    summary.snapshot_will_queue = snapshot_hyperlink_ids.len();
    summary.og_will_queue = og_hyperlink_ids.len();
    summary.readability_will_queue = readability_hyperlink_ids.len();

    Ok(MissingArtifactsPlan {
        summary,
        snapshot_hyperlink_ids,
        screenshot_hyperlink_ids,
        og_hyperlink_ids,
        readability_hyperlink_ids,
    })
}

pub(super) async fn execute_missing_artifacts_plan(
    connection: &DatabaseConnection,
    queue: Option<&ProcessingQueueSender>,
    artifact_settings: &ArtifactCollectionSettings,
    plan: MissingArtifactsPlan,
) -> Result<LastRunSummary, DbErr> {
    let snapshot_queued = if artifact_settings.collect_source {
        enqueue_hyperlink_jobs(
            connection,
            queue,
            HyperlinkProcessingJobKind::Snapshot,
            &plan.snapshot_hyperlink_ids,
        )
        .await?
    } else {
        0
    };
    let og_queued = if artifact_settings.collect_og {
        enqueue_hyperlink_jobs(
            connection,
            queue,
            HyperlinkProcessingJobKind::Og,
            &plan.og_hyperlink_ids,
        )
        .await?
    } else {
        0
    };
    let readability_queued = if artifact_settings.collect_readability {
        enqueue_hyperlink_jobs(
            connection,
            queue,
            HyperlinkProcessingJobKind::Readability,
            &plan.readability_hyperlink_ids,
        )
        .await?
    } else {
        0
    };

    Ok(LastRunSummary {
        snapshot_queued,
        og_queued,
        readability_queued,
    })
}

pub(super) async fn build_missing_screenshot_hyperlink_ids(
    connection: &DatabaseConnection,
) -> Result<Vec<i32>, DbErr> {
    let plan = build_missing_artifacts_plan(
        connection,
        &ArtifactCollectionSettings {
            collect_source: true,
            collect_screenshots: true,
            collect_screenshot_dark: true,
            collect_og: false,
            collect_readability: false,
        },
    )
    .await?;
    Ok(plan.screenshot_hyperlink_ids)
}
