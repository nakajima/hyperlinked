use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "rss_feed")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(unique, indexed)]
    pub url: String,
    pub title: String,
    pub site_url: Option<String>,
    #[sea_orm(default_value = true, indexed)]
    pub active: bool,
    #[sea_orm(default_value = 1800)]
    pub poll_interval_secs: i32,
    #[sea_orm(indexed)]
    pub last_fetched_at: Option<DateTime>,
    #[sea_orm(indexed)]
    pub created_at: DateTime,
    pub updated_at: DateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::hyperlink::Entity")]
    Hyperlink,
}

impl Related<super::hyperlink::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Hyperlink.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
