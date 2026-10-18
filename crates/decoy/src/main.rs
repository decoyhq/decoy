use std::path::PathBuf;

use clap::{Parser, Subcommand};
use decoy_runtime::config::{ServeCliOptions, ServeConfig};

mod upgrade;

#[derive(Debug, Parser)]
#[command(name = "decoy", version)]
#[command(about = "Decoy native runtime")]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Without a subcommand: `serve` plus the interactive terminal UI
    #[command(flatten)]
    serve: ServeArgs,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Serve the mocks headless
    Serve(ServeArgs),
    /// Replace this binary with the newest (or given) release
    Upgrade {
        /// Version to install, e.g. 0.1.0-alpha.6 (default: newest release)
        version: Option<String>,
    },
}

#[derive(Debug, clap::Args)]
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
    let cli = Cli::parse();
    match cli.command {
        None => tui(cli.serve).await?,
        Some(Command::Serve(args)) => serve(args).await?,
        Some(Command::Upgrade { version }) => upgrade::run(version.as_deref())?,
    }

    Ok(())
}

async fn tui(args: ServeArgs) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        eprintln!("decoy: no terminal; use `decoy serve`");
        std::process::exit(1);
    }
    let (quit, quitted) = tokio::sync::oneshot::channel();
    let shutdown = async {
        tokio::select! {
            _ = quitted => {}
            () = shutdown_signal() => {}
        }
    };
    let server = decoy_runtime::serve(serve_config(args)?, shutdown)
        .await
        .map_err(|error| error as Box<dyn std::error::Error>)?;
    decoy_tui::run(server, quit)
        .await
        .map_err(|error| error as Box<dyn std::error::Error>)
}

async fn serve(args: ServeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let config = serve_config(args)?;
    let server = decoy_runtime::serve(config, shutdown_signal())
        .await
        .map_err(|error| error as Box<dyn std::error::Error>)?;
    for notice in &server.notices {
        eprintln!("decoy serve: {notice}");
    }
    for listener in &server.listeners {
        eprintln!("decoy serve listening on {}", listener.label());
    }
    server
        .wait()
        .await
        .map_err(|error| error as Box<dyn std::error::Error>)
}

fn serve_config(args: ServeArgs) -> Result<ServeConfig, Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()?;
    Ok(ServeConfig::from_options(
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
    )?)
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
