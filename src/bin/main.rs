use clap::{Parser, Subcommand};
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

        #[arg(long, default_value_t = true)]
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

        #[arg(long, default_value_t = true)]
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
