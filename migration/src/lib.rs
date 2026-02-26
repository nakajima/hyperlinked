pub use sea_orm_migration::prelude::*;

mod m20220101_000001_create_table;
mod m20260218_000002_add_hyperlink_url_unique;
mod m20260219_000003_add_hyperlink_clicks_count;
mod m20260219_000004_add_hyperlink_last_clicked_at;
mod m20260219_000005_add_hyperlink_processing_state;
mod m20260219_000006_create_hyperlink_processing_error;
mod m20260219_000007_add_processing_jobs_and_snapshots;
mod m20260219_000008_add_job_kinds_and_artifacts;
mod m20260220_000009_add_hyperlink_discovery_relations;
mod m20260221_000010_add_hyperlink_raw_url_and_canonicalize;
mod m20260222_000011_add_hyperlink_search_fts;
mod m20260222_000012_add_artifact_file_storage_and_screenshots;
mod m20260222_000013_add_hyperlink_processing_job_active_unique_guard;
mod m20260222_000014_add_hyperlink_og_fields;
mod m20260224_000015_add_hyperlink_index_pagination_indexes;
mod m20260226_000016_add_hyperlink_tombstones;
mod m20260226_000017_add_hyperlink_artifact_size_index;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20220101_000001_create_table::Migration),
            Box::new(m20260218_000002_add_hyperlink_url_unique::Migration),
            Box::new(m20260219_000003_add_hyperlink_clicks_count::Migration),
            Box::new(m20260219_000004_add_hyperlink_last_clicked_at::Migration),
            Box::new(m20260219_000005_add_hyperlink_processing_state::Migration),
            Box::new(m20260219_000006_create_hyperlink_processing_error::Migration),
            Box::new(m20260219_000007_add_processing_jobs_and_snapshots::Migration),
            Box::new(m20260219_000008_add_job_kinds_and_artifacts::Migration),
            Box::new(m20260220_000009_add_hyperlink_discovery_relations::Migration),
            Box::new(m20260221_000010_add_hyperlink_raw_url_and_canonicalize::Migration),
            Box::new(m20260222_000011_add_hyperlink_search_fts::Migration),
            Box::new(m20260222_000012_add_artifact_file_storage_and_screenshots::Migration),
            Box::new(m20260222_000013_add_hyperlink_processing_job_active_unique_guard::Migration),
            Box::new(m20260222_000014_add_hyperlink_og_fields::Migration),
            Box::new(m20260224_000015_add_hyperlink_index_pagination_indexes::Migration),
            Box::new(m20260226_000016_add_hyperlink_tombstones::Migration),
            Box::new(m20260226_000017_add_hyperlink_artifact_size_index::Migration),
        ]
    }
}
