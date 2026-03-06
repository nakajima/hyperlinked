use sea_orm::{
    ActiveModelTrait,
    ActiveValue::Set,
    ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, TransactionTrait,
    entity::prelude::{DateTime, DateTimeUtc},
    sea_query::Expr,
};
use serde::Deserialize;

use crate::{
    entity::hyperlink_processing_job::HyperlinkProcessingJobKind,
    entity::hyperlink,
    model::{
        hyperlink_processing_job::{self, ProcessingQueueSender},
        settings, tagging_settings, url_canonicalize,
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

#[derive(Clone, Debug, Default)]
pub struct TitleBackfillReport {
    pub scanned: usize,
    pub updated: usize,
    pub unchanged: usize,
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

pub async fn list_updated_after(
    connection: &impl ConnectionTrait,
    updated_after: DateTime,
) -> Result<Vec<hyperlink::Model>, sea_orm::DbErr> {
    hyperlink::Entity::find()
        .filter(hyperlink::Column::UpdatedAt.gt(updated_after))
        .order_by_asc(hyperlink::Column::UpdatedAt)
        .order_by_asc(hyperlink::Column::Id)
        .all(connection)
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
) -> Result<Option<(hyperlink::Model, bool)>, sea_orm::DbErr> {
    let Some(model) = hyperlink::Entity::find_by_id(id).one(connection).await? else {
        return Ok(None);
    };

    let settings = settings::load(connection).await?;
    let tagging_settings = tagging_settings::load(connection).await?;
    let mut queued = false;
    if let Some(queue) = processing_queue {
        if settings.collect_source {
            hyperlink_processing_job::enqueue_for_hyperlink(connection, model.id, Some(queue))
                .await?;
            queued = true;
        }
        if tagging_settings.classification_enabled() {
            hyperlink_processing_job::enqueue_for_hyperlink_kind(
                connection,
                model.id,
                HyperlinkProcessingJobKind::TagClassification,
                Some(queue),
            )
            .await?;
            queued = true;
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

pub async fn delete_by_id_with_tombstone(
    connection: &DatabaseConnection,
    id: i32,
) -> Result<bool, sea_orm::DbErr> {
    if hyperlink::Entity::find_by_id(id)
        .one(connection)
        .await?
        .is_none()
    {
        return Ok(false);
    }

    let deleted_at = now_utc();
    let txn = connection.begin().await?;
    crate::model::hyperlink_tombstone::upsert(&txn, id, deleted_at).await?;
    let deleted = hyperlink::Entity::delete_by_id(id).exec(&txn).await?;

    if deleted.rows_affected == 0 {
        txn.rollback().await?;
        return Ok(false);
    }

    txn.commit().await?;
    Ok(true)
}

pub async fn backfill_clean_titles(
    connection: &DatabaseConnection,
    batch_size: u64,
) -> Result<TitleBackfillReport, sea_orm::DbErr> {
    let mut report = TitleBackfillReport::default();
    let mut last_id = 0i32;
    let batch_size = batch_size.clamp(1, 10_000);

    loop {
        let rows = hyperlink::Entity::find()
            .filter(hyperlink::Column::Id.gt(last_id))
            .order_by_asc(hyperlink::Column::Id)
            .limit(batch_size)
            .all(connection)
            .await?;

        if rows.is_empty() {
            break;
        }

        for row in rows {
            last_id = row.id;
            report.scanned += 1;

            let cleaned_title = crate::model::hyperlink_title::strip_site_affixes(
                row.title.as_str(),
                row.url.as_str(),
                row.raw_url.as_str(),
            );
            if cleaned_title == row.title {
                report.unchanged += 1;
                continue;
            }

            let mut active_model: hyperlink::ActiveModel = row.into();
            active_model.title = Set(cleaned_title);
            active_model.updated_at = Set(now_utc());
            active_model.update(connection).await?;
            report.updated += 1;
        }
    }

    Ok(report)
}

async fn enqueue_processing_if_enabled(
    connection: &DatabaseConnection,
    processing_queue: Option<&ProcessingQueueSender>,
    hyperlink_id: i32,
) -> Result<(), sea_orm::DbErr> {
    if let Some(queue) = processing_queue {
        let artifact_settings = settings::load(connection).await?;
        let tagging_settings = tagging_settings::load(connection).await?;
        if artifact_settings.collect_source {
            hyperlink_processing_job::enqueue_for_hyperlink(connection, hyperlink_id, Some(queue))
                .await?;
        }
        if tagging_settings.classification_enabled() {
            hyperlink_processing_job::enqueue_for_hyperlink_kind(
                connection,
                hyperlink_id,
                HyperlinkProcessingJobKind::TagClassification,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{entity::hyperlink_tombstone, server::test_support};

    #[tokio::test]
    async fn delete_by_id_with_tombstone_marks_deletion_once() {
        let connection = test_support::new_memory_connection().await;
        test_support::initialize_hyperlinks_schema(&connection).await;
        test_support::execute_sql(
            &connection,
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES (1, 'Example', 'https://example.com', 'https://example.com', 0, 0, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00');
            "#,
        )
        .await;

        let deleted = delete_by_id_with_tombstone(&connection, 1)
            .await
            .expect("delete should succeed");
        assert!(deleted);

        let link = hyperlink::Entity::find_by_id(1)
            .one(&connection)
            .await
            .expect("select hyperlink should work");
        assert!(link.is_none());

        let tombstone = hyperlink_tombstone::Entity::find_by_id(1)
            .one(&connection)
            .await
            .expect("select tombstone should work");
        assert!(tombstone.is_some(), "expected tombstone row");

        let deleted_again = delete_by_id_with_tombstone(&connection, 1)
            .await
            .expect("second delete should succeed");
        assert!(!deleted_again);
    }
}
