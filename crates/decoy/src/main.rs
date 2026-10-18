use std::path::PathBuf;

use clap::{Parser, Subcommand};
use decoy_runtime::config::{ServeCliOptions, ServeConfig};

mod upgrade;

#[derive(Debug, Parser)]
#[command(name = "decoy", version)]
#[command(about = "Decoy native runtime")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Serve(ServeArgs),
    /// Replace this binary with the newest (or given) release
    Upgrade {
        /// Version to install, e.g. 0.1.0-alpha.6 (default: newest release)
        version: Option<String>,
    },
}

#[derive(Debug, Parser)]
struct ServeArgs {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    routes: Option<PathBuf>,
    #[arg(long)]
    collections: Option<PathBuf>,
    #[arg(long)]
    port: Option<u16>,
    #[arg(long)]
    host: Option<String>,
    #[arg(long)]
    collection: Option<String>,
    #[arg(long, value_name = "URL")]
    passthrough_base_url: Option<String>,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().command {
        Command::Serve(args) => serve(args).await?,
        Command::Upgrade { version } => upgrade::run(version.as_deref())?,
    }

    Ok(())
}

async fn serve(args: ServeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()?;
    let config = ServeConfig::from_options(
        ServeCliOptions {
            config_file: args.config,
            routes: args.routes,
            collections: args.collections,
            port: args.port,
            host: args.host,
            collection: args.collection,
            passthrough_base_url: args.passthrough_base_url,
        },
        &cwd,
    )?;
    decoy_runtime::serve(config, shutdown_signal())
        .await?
        .wait()
        .await
}

/// Ctrl-C, or SIGTERM (what `kill`, Docker and CI runners send).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler installs");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
