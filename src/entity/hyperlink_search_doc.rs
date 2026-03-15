//! `SeaORM` Entity.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "hyperlink_search_doc")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub hyperlink_id: i32,
    pub title: String,
    pub url: String,
    pub readable_text: String,
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
}

impl Related<super::hyperlink::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Hyperlink.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
