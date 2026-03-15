use std::time::{Duration, SystemTime};

use sea_orm::{
    ActiveModelTrait,
    ActiveValue::Set,
    DatabaseConnection, DbErr, EntityTrait, PaginatorTrait, QueryOrder,
    entity::prelude::{DateTime, DateTimeUtc},
};

use crate::entity::llm_interaction;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NewLlmInteraction {
    pub kind: String,
    pub provider: String,
    pub model: String,
    pub endpoint_url: String,
    pub api_kind: String,
    pub hyperlink_id: Option<i32>,
    pub processing_job_id: Option<i32>,
    pub admin_job_kind: Option<String>,
    pub admin_job_id: Option<i64>,
    pub request_body: String,
    pub response_body: Option<String>,
    pub response_status: Option<i32>,
    pub error_message: Option<String>,
    pub duration_ms: Option<i32>,
    pub created_at: Option<DateTime>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LlmInteractionPage {
    pub items: Vec<llm_interaction::Model>,
    pub page: u64,
    pub total_pages: u64,
    pub total_items: u64,
}

pub async fn record(
    connection: &DatabaseConnection,
    interaction: NewLlmInteraction,
) -> Result<llm_interaction::Model, DbErr> {
    llm_interaction::ActiveModel {
        kind: Set(interaction.kind.trim().to_string()),
        provider: Set(interaction.provider.trim().to_string()),
        model: Set(interaction.model.trim().to_string()),
        endpoint_url: Set(interaction.endpoint_url.trim().to_string()),
        api_kind: Set(interaction.api_kind.trim().to_string()),
        hyperlink_id: Set(interaction.hyperlink_id),
        processing_job_id: Set(interaction.processing_job_id),
        admin_job_kind: Set(normalize_optional(interaction.admin_job_kind)),
        admin_job_id: Set(interaction.admin_job_id),
        request_body: Set(interaction.request_body),
        response_body: Set(normalize_optional(interaction.response_body)),
        response_status: Set(interaction.response_status),
        error_message: Set(normalize_optional(interaction.error_message)),
        duration_ms: Set(interaction.duration_ms),
        created_at: Set(interaction.created_at.unwrap_or_else(now_utc)),
        ..Default::default()
    }
    .insert(connection)
    .await
}

pub async fn list_page(
    connection: &DatabaseConnection,
    page: u64,
    per_page: u64,
) -> Result<LlmInteractionPage, DbErr> {
    let per_page = per_page.max(1);
    let paginator = llm_interaction::Entity::find()
        .order_by_desc(llm_interaction::Column::CreatedAt)
        .order_by_desc(llm_interaction::Column::Id)
        .paginate(connection, per_page);
    let total_items = paginator.num_items().await?;
    let total_pages = paginator.num_pages().await?.max(1);
    let page = page.max(1).min(total_pages);
    let items = paginator.fetch_page(page.saturating_sub(1)).await?;

    Ok(LlmInteractionPage {
        items,
        page,
        total_pages,
        total_items,
    })
}

pub async fn list_recent(
    connection: &DatabaseConnection,
    limit: u64,
) -> Result<Vec<llm_interaction::Model>, DbErr> {
    Ok(list_page(connection, 1, limit).await?.items)
}

pub async fn clear_all(connection: &DatabaseConnection) -> Result<u64, DbErr> {
    Ok(llm_interaction::Entity::delete_many()
        .exec(connection)
        .await?
        .rows_affected)
}

pub fn format_request_body(body: &serde_json::Value) -> String {
    serde_json::to_string_pretty(body).unwrap_or_else(|_| body.to_string())
}

pub fn duration_ms(duration: Duration) -> i32 {
    duration.as_millis().min(i32::MAX as u128) as i32
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn now_utc() -> DateTime {
    DateTimeUtc::from(SystemTime::now()).naive_utc()
}
#[cfg(test)]
#[path = "../../../tests/unit/app_models_llm_interaction.rs"]
mod tests;
