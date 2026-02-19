use sea_orm::{
    ActiveModelTrait,
    ActiveValue::Set,
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    entity::prelude::{DateTime, DateTimeUtc},
};

use crate::entity::hyperlink_processing_error;

pub async fn insert_new_attempt(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    error_message: &str,
) -> Result<hyperlink_processing_error::Model, sea_orm::DbErr> {
    let attempt = next_attempt(connection, hyperlink_id).await?;
    hyperlink_processing_error::ActiveModel {
        hyperlink_id: Set(hyperlink_id),
        attempt: Set(attempt),
        error_message: Set(error_message.to_string()),
        created_at: Set(now_utc()),
        ..Default::default()
    }
    .insert(connection)
    .await
}

async fn next_attempt(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
) -> Result<i32, sea_orm::DbErr> {
    let max_attempt = hyperlink_processing_error::Entity::find()
        .filter(hyperlink_processing_error::Column::HyperlinkId.eq(hyperlink_id))
        .order_by_desc(hyperlink_processing_error::Column::Attempt)
        .one(connection)
        .await?
        .map(|model| model.attempt)
        .unwrap_or(0);

    Ok(max_attempt + 1)
}

fn now_utc() -> DateTime {
    DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}
