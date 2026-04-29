use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, entity::prelude::*,
};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "rss_feed")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(unique, indexed)]
    pub url: String,
    pub title: String,
    pub site_url: Option<String>,
    #[sea_orm(default_value = true, indexed)]
    pub active: bool,
    #[sea_orm(default_value = 1800)]
    pub poll_interval_secs: i32,
    #[sea_orm(indexed)]
    pub last_fetched_at: Option<DateTime>,
    #[sea_orm(indexed)]
    pub created_at: DateTime,
    pub updated_at: DateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::hyperlink::Entity")]
    Hyperlink,
}

impl Related<super::hyperlink::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Hyperlink.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

pub async fn create(
    connection: &DatabaseConnection,
    url: &str,
    backfill: bool,
    processing_queue: Option<&super::hyperlink_processing_job::ProcessingQueueSender>,
) -> Result<(Model, crate::import::rss::FeedSyncReport), String> {
    let existing = Entity::find()
        .filter(Column::Url.eq(url))
        .one(connection)
        .await
        .map_err(|err| format!("database error: {err}"))?;

    if existing.is_some() {
        return Err(format!("a feed with URL {url} already exists"));
    }

    let parsed = crate::import::rss::fetch_and_parse(url).await?;

    let now = now_utc();
    let model = ActiveModel {
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

    let report =
        crate::import::rss::sync_feed(connection, &feed, backfill, processing_queue).await?;

    Ok((feed, report))
}

pub async fn list(connection: &DatabaseConnection) -> Result<Vec<Model>, DbErr> {
    Entity::find()
        .order_by_desc(Column::CreatedAt)
        .all(connection)
        .await
}

pub async fn find_by_id(connection: &DatabaseConnection, id: i32) -> Result<Option<Model>, DbErr> {
    Entity::find_by_id(id).one(connection).await
}

pub async fn delete_by_id(connection: &DatabaseConnection, id: i32) -> Result<bool, DbErr> {
    let result = Entity::delete_by_id(id).exec(connection).await?;
    Ok(result.rows_affected > 0)
}

pub async fn toggle_active(
    connection: &DatabaseConnection,
    id: i32,
) -> Result<Option<Model>, DbErr> {
    let Some(feed) = Entity::find_by_id(id).one(connection).await? else {
        return Ok(None);
    };

    let new_active = !feed.active;
    let mut active_model: ActiveModel = feed.into();
    active_model.active = Set(new_active);
    active_model.updated_at = Set(now_utc());
    let updated = active_model.update(connection).await?;
    Ok(Some(updated))
}

pub async fn sync_by_id(
    connection: &DatabaseConnection,
    id: i32,
    processing_queue: Option<&super::hyperlink_processing_job::ProcessingQueueSender>,
) -> Result<Option<crate::import::rss::FeedSyncReport>, String> {
    let feed = Entity::find_by_id(id)
        .one(connection)
        .await
        .map_err(|err| format!("database error: {err}"))?;

    let Some(feed) = feed else {
        return Ok(None);
    };

    let report = crate::import::rss::sync_feed(connection, &feed, true, processing_queue).await?;
    Ok(Some(report))
}

pub async fn list_due_for_poll(connection: &DatabaseConnection) -> Result<Vec<Model>, DbErr> {
    let now = now_utc();
    let active_feeds = Entity::find()
        .filter(Column::Active.eq(true))
        .all(connection)
        .await?;

    Ok(active_feeds
        .into_iter()
        .filter(|feed| match feed.last_fetched_at {
            None => true,
            Some(last) => (now - last).num_seconds() >= feed.poll_interval_secs as i64,
        })
        .collect())
}

pub async fn hyperlink_count_for_feed(
    connection: &DatabaseConnection,
    feed_id: i32,
) -> Result<u64, DbErr> {
    super::hyperlink::Entity::find()
        .filter(super::hyperlink::Column::RssFeedId.eq(feed_id))
        .count(connection)
        .await
}

fn now_utc() -> DateTime {
    DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}
