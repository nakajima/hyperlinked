use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, entity::prelude::DateTimeUtc,
};

use crate::{
    app::models::hyperlink_processing_job::ProcessingQueueSender,
    entity::rss_feed,
    import::rss::{self, FeedSyncReport},
};

pub async fn create(
    connection: &DatabaseConnection,
    url: &str,
    backfill: bool,
    processing_queue: Option<&ProcessingQueueSender>,
) -> Result<(rss_feed::Model, FeedSyncReport), String> {
    // Check for duplicate
    let existing = rss_feed::Entity::find()
        .filter(rss_feed::Column::Url.eq(url))
        .one(connection)
        .await
        .map_err(|err| format!("database error: {err}"))?;

    if existing.is_some() {
        return Err(format!("a feed with URL {url} already exists"));
    }

    // Fetch the feed to get title/site_url
    let parsed = rss::fetch_and_parse(url).await?;

    let now = now_utc();
    let model = rss_feed::ActiveModel {
        url: Set(url.to_string()),
        title: Set(parsed.title.clone()),
        site_url: Set(parsed.site_url.clone()),
        active: Set(true),
        poll_interval_secs: Set(1800),
        last_fetched_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };

    let feed = model
        .insert(connection)
        .await
        .map_err(|err| format!("failed to save feed: {err}"))?;

    let report = rss::sync_feed(connection, &feed, backfill, processing_queue).await?;

    Ok((feed, report))
}

pub async fn list(connection: &DatabaseConnection) -> Result<Vec<rss_feed::Model>, sea_orm::DbErr> {
    rss_feed::Entity::find()
        .order_by_desc(rss_feed::Column::CreatedAt)
        .all(connection)
        .await
}

pub async fn find_by_id(
    connection: &DatabaseConnection,
    id: i32,
) -> Result<Option<rss_feed::Model>, sea_orm::DbErr> {
    rss_feed::Entity::find_by_id(id).one(connection).await
}

pub async fn delete_by_id(
    connection: &DatabaseConnection,
    id: i32,
) -> Result<bool, sea_orm::DbErr> {
    let result = rss_feed::Entity::delete_by_id(id).exec(connection).await?;
    Ok(result.rows_affected > 0)
}

pub async fn toggle_active(
    connection: &DatabaseConnection,
    id: i32,
) -> Result<Option<rss_feed::Model>, sea_orm::DbErr> {
    let Some(feed) = rss_feed::Entity::find_by_id(id).one(connection).await? else {
        return Ok(None);
    };

    let new_active = !feed.active;
    let mut active_model: rss_feed::ActiveModel = feed.into();
    active_model.active = Set(new_active);
    active_model.updated_at = Set(now_utc());
    let updated = active_model.update(connection).await?;
    Ok(Some(updated))
}

pub async fn sync_by_id(
    connection: &DatabaseConnection,
    id: i32,
    processing_queue: Option<&ProcessingQueueSender>,
) -> Result<Option<FeedSyncReport>, String> {
    let feed = rss_feed::Entity::find_by_id(id)
        .one(connection)
        .await
        .map_err(|err| format!("database error: {err}"))?;

    let Some(feed) = feed else {
        return Ok(None);
    };

    let report = rss::sync_feed(connection, &feed, true, processing_queue).await?;
    Ok(Some(report))
}

/// Returns feeds that are active and due for a poll.
pub async fn list_due_for_poll(
    connection: &DatabaseConnection,
) -> Result<Vec<rss_feed::Model>, sea_orm::DbErr> {
    let now = now_utc();

    // Get all active feeds, then filter in Rust since SQLite date arithmetic
    // is cumbersome with SeaORM.
    let active_feeds = rss_feed::Entity::find()
        .filter(rss_feed::Column::Active.eq(true))
        .all(connection)
        .await?;

    Ok(active_feeds
        .into_iter()
        .filter(|feed| {
            match feed.last_fetched_at {
                None => true, // Never fetched
                Some(last) => {
                    let elapsed = (now - last).num_seconds();
                    elapsed >= feed.poll_interval_secs as i64
                }
            }
        })
        .collect())
}

pub async fn hyperlink_count_for_feed(
    connection: &DatabaseConnection,
    feed_id: i32,
) -> Result<u64, sea_orm::DbErr> {
    use crate::entity::hyperlink;
    use sea_orm::PaginatorTrait;

    hyperlink::Entity::find()
        .filter(hyperlink::Column::RssFeedId.eq(feed_id))
        .count(connection)
        .await
}

fn now_utc() -> sea_orm::entity::prelude::DateTime {
    DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}
