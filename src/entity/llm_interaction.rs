//! `SeaORM` Entity.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "llm_interaction")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(indexed)]
    pub kind: String,
    pub provider: String,
    pub model: String,
    pub endpoint_url: String,
    pub api_kind: String,
    #[sea_orm(indexed)]
    pub hyperlink_id: Option<i32>,
    #[sea_orm(indexed)]
    pub processing_job_id: Option<i32>,
    pub admin_job_kind: Option<String>,
    pub admin_job_id: Option<i64>,
    pub request_body: String,
    pub response_body: Option<String>,
    pub response_status: Option<i32>,
    pub error_message: Option<String>,
    pub duration_ms: Option<i32>,
    #[sea_orm(indexed)]
    pub created_at: DateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::hyperlink::Entity",
        from = "Column::HyperlinkId",
        to = "super::hyperlink::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Hyperlink,
    #[sea_orm(
        belongs_to = "super::hyperlink_processing_job::Entity",
        from = "Column::ProcessingJobId",
        to = "super::hyperlink_processing_job::Column::Id",
        on_update = "NoAction",
        on_delete = "SetNull"
    )]
    HyperlinkProcessingJob,
}

impl Related<super::hyperlink::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Hyperlink.def()
    }
}

impl Related<super::hyperlink_processing_job::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::HyperlinkProcessingJob.def()
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelatedEntity)]
pub enum RelatedEntity {
    #[sea_orm(entity = "super::hyperlink::Entity")]
    Hyperlink,
    #[sea_orm(entity = "super::hyperlink_processing_job::Entity")]
    HyperlinkProcessingJob,
}

impl ActiveModelBehavior for ActiveModel {}
