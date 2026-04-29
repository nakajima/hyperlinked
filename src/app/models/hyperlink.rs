use sea_orm::{
    ActiveModelTrait,
    ActiveValue::Set,
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    entity::prelude::{DateTime, DateTimeUtc},
};
use serde::Deserialize;

use crate::{
    app::models::{
        artifact_job::{self, ArtifactFetchMode, ArtifactJobResolveResult},
        hyperlink_processing_job::ProcessingQueueSender,
        settings, url_canonicalize,
    },
    entity::hyperlink,
    entity::hyperlink_processing_job::HyperlinkProcessingJobKind,
};

pub use crate::entity::hyperlink::{
    DISCOVERED_DISCOVERY_DEPTH, ROOT_DISCOVERY_DEPTH, TitleBackfillReport, backfill_clean_titles,
    delete_by_id_with_tombstone, find_by_url, increment_click_count_by_id, list_updated_after,
};

#[derive(Clone, Debug, Deserialize)]
pub struct HyperlinkInput {
    pub title: String,
    pub url: String,
}

#[derive(Clone, Debug)]
pub struct NormalizedHyperlinkInput {
    pub title: String,
    pub url: String,
    pub raw_url: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpsertResult {
    Inserted,
    Updated,
}

pub async fn validate_and_normalize(
    input: HyperlinkInput,
) -> Result<NormalizedHyperlinkInput, String> {
    let canonicalized = url_canonicalize::canonicalize_submitted_url(&input.url)?;
    let mut title = input.title.trim().to_string();
    if title.is_empty() {
        title = canonicalized.canonical_url.clone();
    }

    Ok(NormalizedHyperlinkInput {
        title,
        url: canonicalized.canonical_url,
        raw_url: canonicalized.raw_url,
    })
}

pub async fn insert(
    connection: &DatabaseConnection,
    input: NormalizedHyperlinkInput,
    processing_queue: Option<&ProcessingQueueSender>,
) -> Result<hyperlink::Model, sea_orm::DbErr> {
    if let Some(existing) = find_by_url(connection, &input.url).await? {
        return promote_existing_to_root(connection, existing, input, None).await;
    }

    insert_with_created_at_and_depth(
        connection,
        input,
        None,
        ROOT_DISCOVERY_DEPTH,
        processing_queue,
    )
    .await
}

pub async fn insert_with_created_at(
    connection: &DatabaseConnection,
    input: NormalizedHyperlinkInput,
    created_at: Option<DateTime>,
    processing_queue: Option<&ProcessingQueueSender>,
) -> Result<hyperlink::Model, sea_orm::DbErr> {
    if let Some(existing) = find_by_url(connection, &input.url).await? {
        return promote_existing_to_root(connection, existing, input, created_at).await;
    }

    insert_with_created_at_and_depth(
        connection,
        input,
        created_at,
        ROOT_DISCOVERY_DEPTH,
        processing_queue,
    )
    .await
}

pub async fn insert_discovered(
    connection: &DatabaseConnection,
    input: NormalizedHyperlinkInput,
    processing_queue: Option<&ProcessingQueueSender>,
) -> Result<hyperlink::Model, sea_orm::DbErr> {
    let url = input.url.clone();
    match insert_with_created_at_and_depth(
        connection,
        input,
        None,
        DISCOVERED_DISCOVERY_DEPTH,
        processing_queue,
    )
    .await
    {
        Ok(model) => Ok(model),
        Err(err) => {
            if let Some(existing) = find_by_url(connection, &url).await? {
                Ok(existing)
            } else {
                Err(err)
            }
        }
    }
}

async fn insert_with_created_at_and_depth(
    connection: &DatabaseConnection,
    input: NormalizedHyperlinkInput,
    created_at: Option<DateTime>,
    discovery_depth: i32,
    processing_queue: Option<&ProcessingQueueSender>,
) -> Result<hyperlink::Model, sea_orm::DbErr> {
    let now = now_utc();
    let created_at = created_at.unwrap_or(now);
    let model = hyperlink::ActiveModel {
        title: Set(input.title),
        url: Set(input.url),
        raw_url: Set(input.raw_url),
        discovery_depth: Set(discovery_depth),
        clicks_count: Set(0),
        created_at: Set(created_at),
        updated_at: Set(now),
        ..Default::default()
    };
    let inserted = model.insert(connection).await?;
    enqueue_processing_if_enabled(connection, processing_queue, inserted.id).await?;
    Ok(inserted)
}

pub async fn update_by_id(
    connection: &DatabaseConnection,
    id: i32,
    input: NormalizedHyperlinkInput,
    processing_queue: Option<&ProcessingQueueSender>,
) -> Result<Option<hyperlink::Model>, sea_orm::DbErr> {
    let Some(model) = hyperlink::Entity::find_by_id(id).one(connection).await? else {
        return Ok(None);
    };

    let url_changed = model.url != input.url;
    let mut active_model: hyperlink::ActiveModel = model.into();
    active_model.title = Set(input.title);
    active_model.url = Set(input.url);
    active_model.raw_url = Set(input.raw_url);
    active_model.updated_at = Set(now_utc());
    let updated = active_model.update(connection).await?;
    if url_changed {
        enqueue_processing_if_enabled(connection, processing_queue, updated.id).await?;
    }
    Ok(Some(updated))
}

pub async fn enqueue_reprocess_by_id(
    connection: &DatabaseConnection,
    id: i32,
    processing_queue: Option<&ProcessingQueueSender>,
) -> Result<Option<(hyperlink::Model, bool)>, sea_orm::DbErr> {
    let Some(model) = hyperlink::Entity::find_by_id(id).one(connection).await? else {
        return Ok(None);
    };

    let settings = settings::load(connection).await?;
    let mut queued = false;
    if let Some(queue) = processing_queue {
        if settings.collect_source {
            let result = artifact_job::resolve_and_enqueue_for_job_kind_with_settings(
                connection,
                model.id,
                HyperlinkProcessingJobKind::Snapshot,
                ArtifactFetchMode::RefetchTarget,
                settings,
                Some(queue),
            )
            .await?;
            if artifact_job_was_enqueued(result) {
                queued = true;
            }
        }
    }

    Ok(Some((model, queued)))
}

pub async fn upsert_by_url(
    connection: &DatabaseConnection,
    input: NormalizedHyperlinkInput,
    created_at: Option<DateTime>,
    processing_queue: Option<&ProcessingQueueSender>,
) -> Result<UpsertResult, sea_orm::DbErr> {
    let existing = hyperlink::Entity::find()
        .filter(hyperlink::Column::Url.eq(input.url.clone()))
        .order_by_asc(hyperlink::Column::Id)
        .one(connection)
        .await?;

    if let Some(model) = existing {
        promote_existing_to_root(connection, model, input, created_at).await?;
        Ok(UpsertResult::Updated)
    } else {
        insert_with_created_at(connection, input, created_at, processing_queue).await?;
        Ok(UpsertResult::Inserted)
    }
}

async fn promote_existing_to_root(
    connection: &DatabaseConnection,
    model: hyperlink::Model,
    input: NormalizedHyperlinkInput,
    created_at: Option<DateTime>,
) -> Result<hyperlink::Model, sea_orm::DbErr> {
    let mut active_model: hyperlink::ActiveModel = model.into();
    active_model.title = Set(input.title);
    active_model.url = Set(input.url);
    active_model.raw_url = Set(input.raw_url);
    active_model.discovery_depth = Set(ROOT_DISCOVERY_DEPTH);
    if let Some(created_at) = created_at {
        active_model.created_at = Set(created_at);
    }
    active_model.updated_at = Set(now_utc());
    active_model.update(connection).await
}

async fn enqueue_processing_if_enabled(
    connection: &DatabaseConnection,
    processing_queue: Option<&ProcessingQueueSender>,
    hyperlink_id: i32,
) -> Result<(), sea_orm::DbErr> {
    if let Some(queue) = processing_queue {
        let artifact_settings = settings::load(connection).await?;
        if artifact_settings.collect_source {
            let _ = artifact_job::resolve_and_enqueue_for_job_kind_with_settings(
                connection,
                hyperlink_id,
                HyperlinkProcessingJobKind::Snapshot,
                ArtifactFetchMode::RefetchTarget,
                artifact_settings,
                Some(queue),
            )
            .await?;
        }
    }
    Ok(())
}

fn now_utc() -> DateTime {
    DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}

fn artifact_job_was_enqueued(result: ArtifactJobResolveResult) -> bool {
    result.was_enqueued()
}
#[cfg(test)]
#[path = "../../../tests/unit/app_models_hyperlink.rs"]
mod tests;
