//! `SeaORM` Entity.

use sea_orm::entity::prelude::*;

#[derive(
    Clone, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum, serde::Serialize, serde::Deserialize,
)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
pub enum HyperlinkTopicTagSource {
    #[sea_orm(string_value = "USER")]
    User,
    #[sea_orm(string_value = "AI")]
    Ai,
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "hyperlink_topic_tag")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub hyperlink_id: i32,
    pub topic_tag_id: i32,
    pub source: HyperlinkTopicTagSource,
    pub confidence: f32,
    pub rank_index: i32,
    pub created_at: DateTime,
    pub updated_at: DateTime,
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
        belongs_to = "super::topic_tag::Entity",
        from = "Column::TopicTagId",
        to = "super::topic_tag::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    TopicTag,
}

impl Related<super::hyperlink::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Hyperlink.def()
    }
}

impl Related<super::topic_tag::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::TopicTag.def()
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelatedEntity)]
pub enum RelatedEntity {
    #[sea_orm(entity = "super::hyperlink::Entity")]
    Hyperlink,
    #[sea_orm(entity = "super::topic_tag::Entity")]
    TopicTag,
}

impl ActiveModelBehavior for ActiveModel {}
