//! `SeaORM` Entity.

use sea_orm::entity::prelude::*;

#[derive(
    Clone, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum, serde::Serialize, serde::Deserialize,
)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
pub enum ActionTagState {
    #[sea_orm(string_value = "USER")]
    User,
    #[sea_orm(string_value = "AI_APPROVED")]
    AiApproved,
    #[sea_orm(string_value = "AI_PENDING")]
    AiPending,
    #[sea_orm(string_value = "AI_REJECTED")]
    AiRejected,
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "action_tag")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub name_key: String,
    #[sea_orm(default_value = "AI_PENDING")]
    pub state: ActionTagState,
    pub created_at: DateTime,
    pub updated_at: DateTime,
    #[sea_orm(indexed)]
    pub parent_action_tag_id: Option<i32>,
    #[sea_orm(default_value = "")]
    pub path: String,
    #[sea_orm(unique, default_value = "")]
    pub path_key: String,
    #[sea_orm(default_value = 0)]
    pub depth: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::hyperlink_action_tag::Entity")]
    HyperlinkActionTag,
}

impl Related<super::hyperlink_action_tag::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::HyperlinkActionTag.def()
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelatedEntity)]
pub enum RelatedEntity {
    #[sea_orm(entity = "super::hyperlink_action_tag::Entity")]
    HyperlinkActionTag,
}

impl ActiveModelBehavior for ActiveModel {}
