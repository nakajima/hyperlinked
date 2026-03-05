use chrono::{DateTime as ChronoDateTime, NaiveDate, Utc};
use clap::{ArgAction, Parser, Subcommand};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use tracing_subscriber::{EnvFilter, prelude::*};
use tracing_tree::HierarchicalLayer;

const AUTO_MIGRATE_ON_SERVE_ENV: &str = "HYPERLINKED_AUTO_MIGRATE_ON_SERVE";
const DEV_MODE_ENV: &str = "HYPERLINKED_DEV_MODE";
const PAPERLESS_NGX_BASE_URL_ENV: &str = "PAPERLESS_NGX_BASE_URL";
const PAPERLESS_NGX_TOKEN_ENV: &str = "PAPERLESS_NGX_TOKEN";

#[derive(Debug, Parser)]
#[command(name = "hyperlinked")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Serve {
        #[arg(long, default_value = "0.0.0.0")]
        host: String,

        #[arg(long, env = "PORT", default_value = "8765")]
        port: String,

        #[arg(long, default_value_t = true, action = ArgAction::Set)]
        mdns_enabled: bool,

        #[arg(long)]
        mdns_service_name: Option<String>,

        #[arg(
            long,
            default_value = hyperlinked::server::MdnsOptions::default_service_type()
        )]
        mdns_service_type: String,
    },
    Dev {
        #[arg(long, default_value = "0.0.0.0")]
        host: String,

        #[arg(long, env = "PORT", default_value = "8765")]
        port: String,

        #[arg(long, default_value_t = true, action = ArgAction::Set)]
        mdns_enabled: bool,

        #[arg(long)]
        mdns_service_name: Option<String>,

        #[arg(
            long,
            default_value = hyperlinked::server::MdnsOptions::default_service_type()
        )]
        mdns_service_type: String,
    },
    ImportLinkwarden {
        input: PathBuf,
    },
    ImportPaperlessNgx {
        #[arg(long, env = PAPERLESS_NGX_BASE_URL_ENV)]
        base_url: Option<String>,

        #[arg(long, env = PAPERLESS_NGX_TOKEN_ENV)]
        token: Option<String>,

        #[arg(long)]
        since: Option<String>,

        #[arg(long)]
        page_size: Option<usize>,

        #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
        dry_run: bool,
    },
    ArtifactsBackfill {
        #[arg(long, default_value_t = 500)]
        batch_size: u64,
    },
    WarcsCompressBackfill {
        #[arg(long, default_value_t = 500)]
        batch_size: u64,
    },
    TitlesBackfill {
        #[arg(long, default_value_t = 500)]
        batch_size: u64,
    },
    ReprocessAllSnapshots,
    ExportGraphqlSchema {
        #[arg(
            long,
            default_value = "hyperlinked/hyperlinked/GraphQL/Schema/schema.graphqls"
        )]
        out: PathBuf,
    },
}

#[tokio::main]
async fn main() {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,sea_orm::driver::sqlx_sqlite=debug"))
        .add_directive("sqlx::query=off".parse().expect("directive should parse"));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            HierarchicalLayer::new(2)
                .with_indent_lines(true)
                .with_indent_amount(2)
                .with_targets(true)
                .with_bracketed_fields(true),
        )
        .init();

    let exit_code = match run().await {
        Ok(code) => code,
        Err(message) => {
            eprintln!("{message}");
            1
        }
    };

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

async fn run() -> Result<i32, String> {
    match Cli::parse().command {
        Commands::Serve {
            host,
            port,
            mdns_enabled,
            mdns_service_name,
            mdns_service_type,
        } => {
            let mdns_options =
                build_mdns_options(mdns_enabled, mdns_service_name, mdns_service_type);
            let connection = hyperlinked::db::connection::init()
                .await
                .map_err(|err| format!("failed to initialize database connection: {err}"))?;

            let auto_migrate_enabled = auto_migrate_on_serve_enabled();
            let dev_mode = running_in_dev_mode();
            if dev_mode {
                tracing::info!("skipping startup migrations in dev mode");
            } else if auto_migrate_enabled {
                tracing::info!("running pending startup migrations");
                hyperlinked::db::migrate::migrate_pending(&connection).await?;
                tracing::info!("startup migrations complete");
            } else {
                tracing::info!(
                    "{AUTO_MIGRATE_ON_SERVE_ENV}=false; skipping startup migrations on serve"
                );
            }

            hyperlinked::server::start(connection, &host, &port, mdns_options).await?;
            Ok(0)
        }
        Commands::Dev {
            host,
            port,
            mdns_enabled,
            mdns_service_name,
            mdns_service_type,
        } => {
            let mdns_options =
                build_mdns_options(mdns_enabled, mdns_service_name, mdns_service_type);
            hyperlinked::dev_reload::run_dev(host, port, mdns_options).await?;
            Ok(0)
        }
        Commands::ImportLinkwarden { input } => run_linkwarden_import(input).await,
        Commands::ImportPaperlessNgx {
            base_url,
            token,
            since,
            page_size,
            dry_run,
        } => run_paperless_ngx_import(base_url, token, since, page_size, dry_run).await,
        Commands::ArtifactsBackfill { batch_size } => run_artifacts_backfill(batch_size).await,
        Commands::WarcsCompressBackfill { batch_size } => {
            run_warcs_compress_backfill(batch_size).await
        }
        Commands::TitlesBackfill { batch_size } => run_titles_backfill(batch_size).await,
        Commands::ReprocessAllSnapshots => run_reprocess_all_snapshots().await,
        Commands::ExportGraphqlSchema { out } => run_export_graphql_schema(out).await,
    }
}

fn build_mdns_options(
    enabled: bool,
    service_name: Option<String>,
    service_type: String,
) -> hyperlinked::server::MdnsOptions {
    let service_name =
        service_name.unwrap_or_else(hyperlinked::server::MdnsOptions::default_service_name);
    hyperlinked::server::MdnsOptions::new(enabled, service_name, service_type)
}

fn auto_migrate_on_serve_enabled() -> bool {
    parse_auto_migrate_on_serve(std::env::var(AUTO_MIGRATE_ON_SERVE_ENV).ok().as_deref())
}

fn parse_auto_migrate_on_serve(raw: Option<&str>) -> bool {
    match raw {
        Some(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        None => true,
    }
}

fn running_in_dev_mode() -> bool {
    std::env::var(DEV_MODE_ENV).ok().as_deref() == Some("1")
}

async fn run_linkwarden_import(input: PathBuf) -> Result<i32, String> {
    let connection = hyperlinked::db::connection::init()
        .await
        .map_err(|err| format!("failed to initialize database connection: {err}"))?;
    let processing_queue = hyperlinked::queue::ProcessingQueue::connect(connection.clone())
        .await
        .map_err(|err| format!("failed to initialize processing queue: {err}"))?;

    let report = hyperlinked::import::linkwarden::import_file(
        &connection,
        &input,
        hyperlinked::import::linkwarden::ImportFormat::Auto,
        Some(&processing_queue),
    )
    .await
    .map_err(|message| format!("linkwarden import failed: {message}"))?;

    for failure in &report.failures {
        eprintln!(
            "row {}: {}\nentry:\n{}\n",
            failure.row, failure.message, failure.entry_json
        );
    }

    println!(
        "imported {} rows: {} inserted, {} updated, {} failed",
        report.summary.total,
        report.summary.inserted,
        report.summary.updated,
        report.summary.failed
    );

    Ok(0)
}

async fn run_paperless_ngx_import(
    base_url: Option<String>,
    token: Option<String>,
    since: Option<String>,
    page_size: Option<usize>,
    dry_run: bool,
) -> Result<i32, String> {
    let base_url = base_url
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            format!(
                "paperless base url is required (pass --base-url or set {PAPERLESS_NGX_BASE_URL_ENV})"
            )
        })?;
    let token = token
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            format!("paperless token is required (pass --token or set {PAPERLESS_NGX_TOKEN_ENV})")
        })?;
    let since = parse_paperless_since_filter(since.as_deref())?;
    if page_size.is_some_and(|value| value == 0 || value > 1_000) {
        return Err("--page-size must be between 1 and 1000".to_string());
    }

    let connection = hyperlinked::db::connection::init()
        .await
        .map_err(|err| format!("failed to initialize database connection: {err}"))?;
    let processing_queue = if dry_run {
        None
    } else {
        Some(
            hyperlinked::queue::ProcessingQueue::connect(connection.clone())
                .await
                .map_err(|err| format!("failed to initialize processing queue: {err}"))?,
        )
    };

    let report = hyperlinked::import::paperless_ngx::import_from_api(
        &connection,
        hyperlinked::import::paperless_ngx::ImportOptions {
            base_url,
            api_token: token,
            since,
            page_size,
            dry_run,
        },
        processing_queue.as_ref(),
    )
    .await
    .map_err(|message| format!("paperless import failed: {message}"))?;

    for failure in &report.failures {
        let id = failure
            .document_id
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        eprintln!(
            "document {}: {}\nentry:\n{}\n",
            id, failure.message, failure.document_json
        );
    }

    if dry_run {
        println!("dry-run enabled; no hyperlinks or artifacts were written");
    }

    println!(
        "scanned {} documents: {} imported, {} duplicate, {} non-pdf, {} before-since, {} failed",
        report.summary.scanned,
        report.summary.imported,
        report.summary.skipped_duplicate,
        report.summary.skipped_non_pdf,
        report.summary.skipped_before_since,
        report.summary.failed
    );

    Ok(0)
}

fn parse_paperless_since_filter(raw: Option<&str>) -> Result<Option<ChronoDateTime<Utc>>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    if let Ok(value) = ChronoDateTime::parse_from_rfc3339(trimmed) {
        return Ok(Some(value.with_timezone(&Utc)));
    }

    if let Ok(value) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        let Some(naive) = value.and_hms_opt(0, 0, 0) else {
            return Err(format!("invalid --since date value: '{trimmed}'"));
        };
        return Ok(Some(ChronoDateTime::from_naive_utc_and_offset(naive, Utc)));
    }

    Err(format!(
        "invalid --since value '{trimmed}' (expected RFC3339 or YYYY-MM-DD)"
    ))
}

async fn run_artifacts_backfill(batch_size: u64) -> Result<i32, String> {
    let connection = hyperlinked::db::connection::init()
        .await
        .map_err(|err| format!("failed to initialize database connection: {err}"))?;

    let report = hyperlinked::model::hyperlink_artifact::backfill_blob_payloads_to_disk(
        &connection,
        batch_size,
    )
    .await
    .map_err(|err| format!("artifact backfill failed: {err}"))?;

    println!(
        "artifact backfill: scanned={}, migrated={}, skipped_without_payload={}",
        report.scanned, report.migrated, report.skipped_without_payload
    );

    Ok(0)
}

async fn run_warcs_compress_backfill(batch_size: u64) -> Result<i32, String> {
    let connection = hyperlinked::db::connection::init()
        .await
        .map_err(|err| format!("failed to initialize database connection: {err}"))?;

    let report = hyperlinked::model::hyperlink_artifact::backfill_snapshot_warc_payloads_to_gzip(
        &connection,
        batch_size,
    )
    .await
    .map_err(|err| format!("warc compression backfill failed: {err}"))?;

    println!(
        "warc compression backfill: scanned={}, compressed={}, skipped_already_compressed={}, failed={}",
        report.scanned, report.compressed, report.skipped_already_compressed, report.failed
    );

    Ok(0)
}

async fn run_titles_backfill(batch_size: u64) -> Result<i32, String> {
    let connection = hyperlinked::db::connection::init()
        .await
        .map_err(|err| format!("failed to initialize database connection: {err}"))?;

    let report = hyperlinked::model::hyperlink::backfill_clean_titles(&connection, batch_size)
        .await
        .map_err(|err| format!("title backfill failed: {err}"))?;

    println!(
        "title backfill: scanned={}, updated={}, unchanged={}",
        report.scanned, report.updated, report.unchanged
    );

    Ok(0)
}

async fn run_reprocess_all_snapshots() -> Result<i32, String> {
    use hyperlinked::entity::{
        hyperlink,
        hyperlink_processing_job::{self, HyperlinkProcessingJobKind, HyperlinkProcessingJobState},
    };
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};

    let connection = hyperlinked::db::connection::init()
        .await
        .map_err(|err| format!("failed to initialize database connection: {err}"))?;
    let processing_queue = hyperlinked::queue::ProcessingQueue::connect(connection.clone())
        .await
        .map_err(|err| format!("failed to initialize processing queue: {err}"))?;

    let hyperlink_ids = hyperlink::Entity::find()
        .select_only()
        .column(hyperlink::Column::Id)
        .into_tuple::<i32>()
        .all(&connection)
        .await
        .map_err(|err| format!("failed to load hyperlinks: {err}"))?;

    let total_hyperlinks = hyperlink_ids.len();
    if total_hyperlinks == 0 {
        println!("snapshot reprocess: total_links=0 queued=0 skipped_active=0");
        return Ok(0);
    }

    let active_snapshot_hyperlink_ids = hyperlink_processing_job::Entity::find()
        .select_only()
        .column(hyperlink_processing_job::Column::HyperlinkId)
        .filter(hyperlink_processing_job::Column::HyperlinkId.is_in(hyperlink_ids.clone()))
        .filter(hyperlink_processing_job::Column::Kind.eq(HyperlinkProcessingJobKind::Snapshot))
        .filter(hyperlink_processing_job::Column::State.is_in([
            HyperlinkProcessingJobState::Queued,
            HyperlinkProcessingJobState::Running,
        ]))
        .into_tuple::<i32>()
        .all(&connection)
        .await
        .map_err(|err| format!("failed to load active snapshot jobs: {err}"))?
        .into_iter()
        .collect::<HashSet<_>>();

    let mut queued = 0usize;
    let mut skipped_active = 0usize;
    for hyperlink_id in hyperlink_ids {
        if active_snapshot_hyperlink_ids.contains(&hyperlink_id) {
            skipped_active += 1;
            continue;
        }

        hyperlinked::model::hyperlink_processing_job::enqueue_for_hyperlink_kind(
            &connection,
            hyperlink_id,
            HyperlinkProcessingJobKind::Snapshot,
            Some(&processing_queue),
        )
        .await
        .map_err(|err| {
            format!("failed to enqueue snapshot job for hyperlink {hyperlink_id}: {err}")
        })?;
        queued += 1;
    }

    println!(
        "snapshot reprocess: total_links={total_hyperlinks} queued={queued} skipped_active={skipped_active}"
    );

    Ok(0)
}

async fn run_export_graphql_schema(out: PathBuf) -> Result<i32, String> {
    let connection = hyperlinked::db::connection::init()
        .await
        .map_err(|err| format!("failed to initialize database connection: {err}"))?;

    let sdl = hyperlinked::server::graphql::export_schema_sdl(connection)
        .map_err(|err| format!("failed to export graphql schema: {err}"))?;

    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            format!(
                "failed to create schema output directory {}: {err}",
                parent.display()
            )
        })?;
    }

    fs::write(&out, sdl)
        .map_err(|err| format!("failed to write schema file {}: {err}", out.display()))?;
    println!("wrote graphql schema to {}", out.display());
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::parse_auto_migrate_on_serve;

    #[test]
    fn auto_migrate_defaults_to_enabled() {
        assert!(parse_auto_migrate_on_serve(None));
    }

    #[test]
    fn auto_migrate_disable_values_are_honored() {
        for value in ["0", "false", "no", "off", " FALSE ", "Off"] {
            assert!(!parse_auto_migrate_on_serve(Some(value)));
        }
    }

    #[test]
    fn auto_migrate_non_disable_values_enable_migration() {
        for value in ["1", "true", "yes", "on", "custom", ""] {
            assert!(parse_auto_migrate_on_serve(Some(value)));
        }
    }
}
