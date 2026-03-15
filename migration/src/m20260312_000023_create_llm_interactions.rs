use sea_orm_migration::prelude::*;

const LLM_INTERACTION_CREATED_AT_INDEX: &str = "idx_llm_interaction_created_at";
const LLM_INTERACTION_KIND_INDEX: &str = "idx_llm_interaction_kind";
const LLM_INTERACTION_HYPERLINK_ID_INDEX: &str = "idx_llm_interaction_hyperlink_id";
const LLM_INTERACTION_PROCESSING_JOB_ID_INDEX: &str = "idx_llm_interaction_processing_job_id";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(LlmInteraction::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(LlmInteraction::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(LlmInteraction::Kind).string().not_null())
                    .col(ColumnDef::new(LlmInteraction::Provider).string().not_null())
                    .col(ColumnDef::new(LlmInteraction::Model).string().not_null())
                    .col(
                        ColumnDef::new(LlmInteraction::EndpointUrl)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(LlmInteraction::ApiKind).string().not_null())
                    .col(ColumnDef::new(LlmInteraction::HyperlinkId).integer().null())
                    .col(
                        ColumnDef::new(LlmInteraction::ProcessingJobId)
                            .integer()
                            .null(),
                    )
                    .col(ColumnDef::new(LlmInteraction::AdminJobKind).string().null())
                    .col(
                        ColumnDef::new(LlmInteraction::AdminJobId)
                            .big_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(LlmInteraction::RequestBody)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(LlmInteraction::ResponseBody).text().null())
                    .col(
                        ColumnDef::new(LlmInteraction::ResponseStatus)
                            .integer()
                            .null(),
                    )
                    .col(ColumnDef::new(LlmInteraction::ErrorMessage).text().null())
                    .col(ColumnDef::new(LlmInteraction::DurationMs).integer().null())
                    .col(
                        ColumnDef::new(LlmInteraction::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-llm-interaction-hyperlink-id")
                            .from(LlmInteraction::Table, LlmInteraction::HyperlinkId)
                            .to(Hyperlink::Table, Hyperlink::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-llm-interaction-processing-job-id")
                            .from(LlmInteraction::Table, LlmInteraction::ProcessingJobId)
                            .to(HyperlinkProcessingJob::Table, HyperlinkProcessingJob::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name(LLM_INTERACTION_CREATED_AT_INDEX)
                    .table(LlmInteraction::Table)
                    .col(LlmInteraction::CreatedAt)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name(LLM_INTERACTION_KIND_INDEX)
                    .table(LlmInteraction::Table)
                    .col(LlmInteraction::Kind)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name(LLM_INTERACTION_HYPERLINK_ID_INDEX)
                    .table(LlmInteraction::Table)
                    .col(LlmInteraction::HyperlinkId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name(LLM_INTERACTION_PROCESSING_JOB_ID_INDEX)
                    .table(LlmInteraction::Table)
                    .col(LlmInteraction::ProcessingJobId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name(LLM_INTERACTION_PROCESSING_JOB_ID_INDEX)
                    .table(LlmInteraction::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .name(LLM_INTERACTION_HYPERLINK_ID_INDEX)
                    .table(LlmInteraction::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .name(LLM_INTERACTION_KIND_INDEX)
                    .table(LlmInteraction::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .name(LLM_INTERACTION_CREATED_AT_INDEX)
                    .table(LlmInteraction::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(LlmInteraction::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum LlmInteraction {
    Table,
    Id,
    Kind,
    Provider,
    Model,
    EndpointUrl,
    ApiKind,
    HyperlinkId,
    ProcessingJobId,
    AdminJobKind,
    AdminJobId,
    RequestBody,
    ResponseBody,
    ResponseStatus,
    ErrorMessage,
    DurationMs,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Hyperlink {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum HyperlinkProcessingJob {
    Table,
    Id,
}
