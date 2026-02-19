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
    },
    Dev {
        #[arg(long, default_value = "0.0.0.0")]
        host: String,

        #[arg(long, env = "PORT", default_value = "8765")]
        port: String,
    },
    ImportLinkwarden {
        input: PathBuf,
    },
}

#[tokio::main]
async fn main() {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,sea_orm::driver::sqlx_sqlite=debug"))
        .add_directive("sqlx::query=off".parse().expect("directive should parse"));

    _ = tracing_subscriber::registry()
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
        Commands::Serve { host, port } => {
            hyperlinked::server::start(&host, &port).await;
            Ok(0)
        }
        Commands::Dev { host, port } => {
            hyperlinked::dev_reload::run_dev(host, port).await?;
            Ok(0)
        }
        Commands::ImportLinkwarden { input } => run_linkwarden_import(input).await,
    }
}

async fn run_linkwarden_import(input: PathBuf) -> Result<i32, String> {
    let connection = hyperlinked::db::connection::init()
        .await
        .map_err(|err| format!("failed to initialize database connection: {err}"))?;

    let report = hyperlinked::import::linkwarden::import_file(
        &connection,
        &input,
        hyperlinked::import::linkwarden::ImportFormat::Auto,
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
