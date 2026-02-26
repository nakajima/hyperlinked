use clap::{ArgAction, Parser, Subcommand};
use std::collections::HashSet;
use std::path::PathBuf;
use tracing_subscriber::{EnvFilter, prelude::*};
use tracing_tree::HierarchicalLayer;

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
            hyperlinked::server::start(&host, &port, mdns_options).await?;
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
        Commands::ArtifactsBackfill { batch_size } => run_artifacts_backfill(batch_size).await,
        Commands::WarcsCompressBackfill { batch_size } => {
            run_warcs_compress_backfill(batch_size).await
        }
        Commands::TitlesBackfill { batch_size } => run_titles_backfill(batch_size).await,
        Commands::ReprocessAllSnapshots => run_reprocess_all_snapshots().await,
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
