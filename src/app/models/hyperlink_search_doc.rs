use sea_orm::{
    ActiveModelTrait,
    ActiveValue::Set,
    ColumnTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter,
    entity::prelude::{DateTime, DateTimeUtc},
    sea_query::Expr,
};

use crate::entity::{hyperlink, hyperlink_search_doc};

pub async fn upsert_readable_text(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    readable_text: &str,
) -> Result<(), DbErr> {
    let Some(link) = hyperlink::Entity::find_by_id(hyperlink_id)
        .one(connection)
        .await?
    else {
        return Ok(());
    };

    let updated_at = now_utc();
    if let Some(existing) = hyperlink_search_doc::Entity::find_by_id(hyperlink_id)
        .one(connection)
        .await?
    {
        let mut active_model: hyperlink_search_doc::ActiveModel = existing.into();
        active_model.title = Set(link.title);
        active_model.url = Set(link.url);
        active_model.readable_text = Set(readable_text.to_string());
        active_model.updated_at = Set(updated_at);
        active_model.update(connection).await?;
    } else {
        hyperlink_search_doc::ActiveModel {
            hyperlink_id: Set(hyperlink_id),
            title: Set(link.title),
            url: Set(link.url),
            readable_text: Set(readable_text.to_string()),
            updated_at: Set(updated_at),
        }
        .insert(connection)
        .await?;
    }

    Ok(())
}

pub async fn clear_all_readable_text(connection: &DatabaseConnection) -> Result<u64, DbErr> {
    let result = hyperlink_search_doc::Entity::update_many()
        .col_expr(hyperlink_search_doc::Column::ReadableText, Expr::value(""))
        .col_expr(
            hyperlink_search_doc::Column::UpdatedAt,
            Expr::value(now_utc()),
        )
        .filter(hyperlink_search_doc::Column::ReadableText.ne(""))
        .exec(connection)
        .await?;
    Ok(result.rows_affected)
}

pub async fn clear_readable_text_for_hyperlink(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
) -> Result<u64, DbErr> {
    let result = hyperlink_search_doc::Entity::update_many()
        .col_expr(hyperlink_search_doc::Column::ReadableText, Expr::value(""))
        .col_expr(
            hyperlink_search_doc::Column::UpdatedAt,
            Expr::value(now_utc()),
        )
        .filter(hyperlink_search_doc::Column::HyperlinkId.eq(hyperlink_id))
        .filter(hyperlink_search_doc::Column::ReadableText.ne(""))
        .exec(connection)
        .await?;
    Ok(result.rows_affected)
}

pub async fn load_readable_text_excerpt_for_hyperlink(
    connection: &DatabaseConnection,
    hyperlink_id: i32,
    max_chars: usize,
) -> Result<Option<String>, DbErr> {
    let Some(row) = hyperlink_search_doc::Entity::find_by_id(hyperlink_id)
        .one(connection)
        .await?
    else {
        return Ok(None);
    };

    let trimmed = row.readable_text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let excerpt = if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        trimmed.chars().take(max_chars).collect::<String>()
    };
    Ok(Some(excerpt))
}

pub fn is_search_doc_missing_error(error: &DbErr) -> bool {
    match error {
        DbErr::Exec(exec_error) => exec_error
            .to_string()
            .contains("no such table: hyperlink_search_doc"),
        DbErr::Query(query_error) => query_error
            .to_string()
            .contains("no such table: hyperlink_search_doc"),
        _ => false,
    }
}

fn now_utc() -> DateTime {
    DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}

#[cfg(test)]
#[path = "../../../tests/unit/app_models_hyperlink_search_doc.rs"]
mod tests;
