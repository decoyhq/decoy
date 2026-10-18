use std::path::PathBuf;

use clap::{Parser, Subcommand};
use decoy_runtime::config::{ServeCliOptions, ServeConfig};
use decoy_runtime::watch::{ConfigWatcher, WatchedPaths};

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

#[derive(Debug, Clone, clap::Args)]
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
    /// Reload when a config, routes, collections or proto file changes
    #[arg(long)]
    watch: bool,
    /// --watch, polling the files instead of waiting for change events (implies --watch; WSL2
    /// `/mnt/*` paths and network filesystems poll on their own)
    #[arg(long)]
    watch_poll: bool,
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
    let config = serve_config(args.clone())?;
    let watching = watch(&args, &config)?;
    let server = decoy_runtime::serve(config, shutdown)
        .await
        .map_err(|error| error as Box<dyn std::error::Error>)?;
    decoy_tui::run(server, quit, config_reader(args), watching)
        .await
        .map_err(|error| error as Box<dyn std::error::Error>)
}

/// Re-reads the config from the same flags and files, for a reload.
fn config_reader(args: ServeArgs) -> decoy_tui::LoadConfig {
    Box::new(move || serve_config(args.clone()).map_err(|error| error.to_string().into()))
}

/// `--watch`: one signal per burst of changes to the files `config` was read from.
fn watch(
    args: &ServeArgs,
    config: &ServeConfig,
) -> Result<Option<ConfigWatcher>, Box<dyn std::error::Error>> {
    if !args.watch && !args.watch_poll {
        return Ok(None);
    }
    let config_file = match &args.config {
        Some(file) => Some(std::env::current_dir()?.join(file)),
        None => None,
    };
    let paths = WatchedPaths::of(config, config_file.as_deref());
    let poll = args.watch_poll;
    let args = args.clone();
    let read_paths = Box::new(move || {
        let config = serve_config(args.clone()).ok()?;
        Some(WatchedPaths::of(&config, config_file.as_deref()))
    });
    Ok(Some(ConfigWatcher::new(paths, read_paths, poll)?))
}

async fn serve(args: ServeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let config = serve_config(args.clone())?;
    let watching = watch(&args, &config)?;
    let server = decoy_runtime::serve(config, shutdown_signal())
        .await
        .map_err(|error| error as Box<dyn std::error::Error>)?;
    for notice in &server.notices {
        eprintln!("decoy serve: {notice}");
    }
    for listener in &server.listeners {
        eprintln!("decoy serve listening on {}", listener.label());
    }
    if let Some(mut watcher) = watching {
        eprintln!("decoy serve: {}", watcher.describe());
        let reloader = server.reloader();
        let read_config = config_reader(args);
        tokio::spawn(async move {
            while watcher.changed().await.is_some() {
                match reloader.reload(&read_config).await {
                    Ok(reloaded) => {
                        eprintln!("decoy serve: reloaded: {}", reloaded.summary());
                        for warning in reloaded.warnings {
                            eprintln!("decoy serve: {warning}");
                        }
                    }
                    Err(error) => {
                        eprintln!("decoy serve: reload failed, the old config stays live: {error}");
                    }
                }
            }
        });
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
