use sea_orm::{
    ActiveModelTrait,
    ActiveValue::Set,
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    entity::prelude::{DateTime, DateTimeUtc},
    sea_query::Expr,
};
use serde::Deserialize;

use crate::{
    entity::hyperlink,
    model::{
        hyperlink_processing_job::{self, ProcessingQueueSender},
        url_canonicalize,
    },
};

pub const ROOT_DISCOVERY_DEPTH: i32 = 0;
pub const DISCOVERED_DISCOVERY_DEPTH: i32 = 1;

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

pub async fn find_by_url(
    connection: &DatabaseConnection,
    url: &str,
) -> Result<Option<hyperlink::Model>, sea_orm::DbErr> {
    hyperlink::Entity::find()
        .filter(hyperlink::Column::Url.eq(url.to_string()))
        .order_by_asc(hyperlink::Column::Id)
        .one(connection)
        .await
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
) -> Result<Option<hyperlink::Model>, sea_orm::DbErr> {
    let Some(model) = hyperlink::Entity::find_by_id(id).one(connection).await? else {
        return Ok(None);
    };

    if let Some(queue) = processing_queue {
        hyperlink_processing_job::enqueue_for_hyperlink(connection, model.id, Some(queue)).await?;
    }

    Ok(Some(model))
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

pub async fn increment_click_count_by_id(
    connection: &DatabaseConnection,
    id: i32,
) -> Result<Option<hyperlink::Model>, sea_orm::DbErr> {
    let now = now_utc();
    let updated = hyperlink::Entity::update_many()
        .col_expr(
            hyperlink::Column::ClicksCount,
            Expr::col(hyperlink::Column::ClicksCount).add(1),
        )
        .col_expr(hyperlink::Column::LastClickedAt, Expr::val(now).into())
        .filter(hyperlink::Column::Id.eq(id))
        .exec(connection)
        .await?;

    if updated.rows_affected == 0 {
        return Ok(None);
    }

    hyperlink::Entity::find_by_id(id).one(connection).await
}

async fn enqueue_processing_if_enabled(
    connection: &DatabaseConnection,
    processing_queue: Option<&ProcessingQueueSender>,
    hyperlink_id: i32,
) -> Result<(), sea_orm::DbErr> {
    if let Some(queue) = processing_queue {
        hyperlink_processing_job::enqueue_for_hyperlink(connection, hyperlink_id, Some(queue))
            .await?;
    }
    Ok(())
}

fn now_utc() -> DateTime {
    DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}
