pub use sea_orm_migration::prelude::*;

mod m20220101_000001_create_table;
mod m20260218_000002_add_hyperlink_url_unique;
mod m20260219_000003_add_hyperlink_clicks_count;
mod m20260219_000004_add_hyperlink_last_clicked_at;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20220101_000001_create_table::Migration),
            Box::new(m20260218_000002_add_hyperlink_url_unique::Migration),
            Box::new(m20260219_000003_add_hyperlink_clicks_count::Migration),
            Box::new(m20260219_000004_add_hyperlink_last_clicked_at::Migration),
        ]
    }
}
