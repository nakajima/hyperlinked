use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
    QueryOrder, entity::prelude::DateTime,
};

use crate::entity::hyperlink_tombstone;

pub async fn upsert<C: ConnectionTrait>(
    connection: &C,
    hyperlink_id: i32,
    updated_at: DateTime,
) -> Result<(), sea_orm::DbErr> {
    if let Some(existing) = hyperlink_tombstone::Entity::find_by_id(hyperlink_id)
        .one(connection)
        .await?
    {
        let mut active: hyperlink_tombstone::ActiveModel = existing.into();
        active.updated_at = Set(updated_at);
        active.update(connection).await?;
    } else {
        hyperlink_tombstone::ActiveModel {
            hyperlink_id: Set(hyperlink_id),
            updated_at: Set(updated_at),
        }
        .insert(connection)
        .await?;
    }

    Ok(())
}

pub async fn list_updated_after(
    connection: &impl ConnectionTrait,
    updated_after: DateTime,
) -> Result<Vec<hyperlink_tombstone::Model>, sea_orm::DbErr> {
    hyperlink_tombstone::Entity::find()
        .filter(hyperlink_tombstone::Column::UpdatedAt.gt(updated_after))
        .order_by_asc(hyperlink_tombstone::Column::UpdatedAt)
        .order_by_asc(hyperlink_tombstone::Column::HyperlinkId)
        .all(connection)
        .await
}
