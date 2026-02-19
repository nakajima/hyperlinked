use sea_orm::{
    ActiveModelTrait,
    ActiveValue::Set,
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    entity::prelude::{DateTime, DateTimeUtc},
    sea_query::Expr,
};
use serde::Deserialize;

use crate::{entity::hyperlink, processors::title_fetch};

#[derive(Clone, Debug, Deserialize)]
pub struct HyperlinkInput {
    pub title: String,
    pub url: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpsertResult {
    Inserted,
    Updated,
}

pub async fn validate_and_normalize(input: HyperlinkInput) -> Result<HyperlinkInput, String> {
    let (title, url) = title_fetch::normalize_title_and_url(&input.title, &input.url).await?;
    Ok(HyperlinkInput { title, url })
}

pub async fn insert(
    connection: &DatabaseConnection,
    input: HyperlinkInput,
) -> Result<hyperlink::Model, sea_orm::DbErr> {
    insert_with_created_at(connection, input, None).await
}

pub async fn insert_with_created_at(
    connection: &DatabaseConnection,
    input: HyperlinkInput,
    created_at: Option<DateTime>,
) -> Result<hyperlink::Model, sea_orm::DbErr> {
    let now = now_utc();
    let created_at = created_at.unwrap_or(now);
    let model = hyperlink::ActiveModel {
        title: Set(input.title),
        url: Set(input.url),
        clicks_count: Set(0),
        created_at: Set(created_at),
        updated_at: Set(now),
        ..Default::default()
    };
    model.insert(connection).await
}

pub async fn update_by_id(
    connection: &DatabaseConnection,
    id: i32,
    input: HyperlinkInput,
) -> Result<Option<hyperlink::Model>, sea_orm::DbErr> {
    let Some(model) = hyperlink::Entity::find_by_id(id).one(connection).await? else {
        return Ok(None);
    };

    let mut active_model: hyperlink::ActiveModel = model.into();
    active_model.title = Set(input.title);
    active_model.url = Set(input.url);
    active_model.updated_at = Set(now_utc());
    active_model.update(connection).await.map(Some)
}

pub async fn upsert_by_url(
    connection: &DatabaseConnection,
    input: HyperlinkInput,
    created_at: Option<DateTime>,
) -> Result<UpsertResult, sea_orm::DbErr> {
    let existing = hyperlink::Entity::find()
        .filter(hyperlink::Column::Url.eq(input.url.clone()))
        .order_by_asc(hyperlink::Column::Id)
        .one(connection)
        .await?;

    if let Some(model) = existing {
        let mut active_model: hyperlink::ActiveModel = model.into();
        active_model.title = Set(input.title);
        if let Some(created_at) = created_at {
            active_model.created_at = Set(created_at);
        }
        active_model.updated_at = Set(now_utc());
        active_model.update(connection).await?;
        Ok(UpsertResult::Updated)
    } else {
        insert_with_created_at(connection, input, created_at).await?;
        Ok(UpsertResult::Inserted)
    }
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

fn now_utc() -> DateTime {
    DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}
