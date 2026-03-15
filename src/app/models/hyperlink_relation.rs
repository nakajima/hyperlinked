use std::collections::HashMap;

use sea_orm::{
    ActiveModelTrait,
    ActiveValue::Set,
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    entity::prelude::{DateTime, DateTimeUtc},
};

use crate::entity::{hyperlink, hyperlink_relation};

pub async fn link_parent_child(
    connection: &DatabaseConnection,
    parent_hyperlink_id: i32,
    child_hyperlink_id: i32,
) -> Result<hyperlink_relation::Model, sea_orm::DbErr> {
    if parent_hyperlink_id == child_hyperlink_id {
        return Err(sea_orm::DbErr::Custom(
            "parent_hyperlink_id and child_hyperlink_id must differ".to_string(),
        ));
    }

    if let Some(existing) = hyperlink_relation::Entity::find()
        .filter(hyperlink_relation::Column::ParentHyperlinkId.eq(parent_hyperlink_id))
        .filter(hyperlink_relation::Column::ChildHyperlinkId.eq(child_hyperlink_id))
        .one(connection)
        .await?
    {
        return Ok(existing);
    }

    let model = hyperlink_relation::ActiveModel {
        parent_hyperlink_id: Set(parent_hyperlink_id),
        child_hyperlink_id: Set(child_hyperlink_id),
        created_at: Set(now_utc()),
        ..Default::default()
    };

    match model.insert(connection).await {
        Ok(inserted) => Ok(inserted),
        Err(err) => {
            if let Some(existing) = hyperlink_relation::Entity::find()
                .filter(hyperlink_relation::Column::ParentHyperlinkId.eq(parent_hyperlink_id))
                .filter(hyperlink_relation::Column::ChildHyperlinkId.eq(child_hyperlink_id))
                .one(connection)
                .await?
            {
                Ok(existing)
            } else {
                Err(err)
            }
        }
    }
}

pub async fn children_for_parent(
    connection: &DatabaseConnection,
    parent_hyperlink_id: i32,
) -> Result<Vec<hyperlink::Model>, sea_orm::DbErr> {
    let relations = hyperlink_relation::Entity::find()
        .filter(hyperlink_relation::Column::ParentHyperlinkId.eq(parent_hyperlink_id))
        .order_by_desc(hyperlink_relation::Column::CreatedAt)
        .order_by_desc(hyperlink_relation::Column::Id)
        .all(connection)
        .await?;

    if relations.is_empty() {
        return Ok(Vec::new());
    }

    let child_ids = relations
        .iter()
        .map(|relation| relation.child_hyperlink_id)
        .collect::<Vec<_>>();

    let children = hyperlink::Entity::find()
        .filter(hyperlink::Column::Id.is_in(child_ids.clone()))
        .all(connection)
        .await?;
    let mut by_id = children
        .into_iter()
        .map(|child| (child.id, child))
        .collect::<HashMap<_, _>>();

    let mut ordered = Vec::with_capacity(relations.len());
    for relation in relations {
        if let Some(child) = by_id.remove(&relation.child_hyperlink_id) {
            ordered.push(child);
        }
    }

    Ok(ordered)
}

fn now_utc() -> DateTime {
    DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}
