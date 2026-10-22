//! Plugin processes: started with Decoy, one per `plugins:` entry, spoken to over stdin/stdout
//! (`docs/design/plugin-protocol.md`). A plugin exiting stops Decoy; nothing restarts it
//! (ADR 0004).

mod host;
mod protobuf;
pub mod value;
mod wire;

use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use prost_reflect::DescriptorPool;
use thiserror::Error;
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::config::PluginConfig;
pub use host::{Call, CallConnection, CallError, Frame, Op, Plugin};
use host::{Stderr, connect, forward_stderr};
use protobuf::ProtoHost;

/// How long a plugin may take to answer `hello`.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a plugin has to exit after `shutdown` before it is killed.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Why plugins didn't start, or why one stopped.
#[derive(Debug, Error)]
pub enum PluginError {
    #[error("plugin `{key}`: can't run `{command}`: {source}")]
    Spawn {
        key: String,
        command: String,
        source: std::io::Error,
    },
    #[error("plugin `{key}` didn't start: {reason}{tail}")]
    Handshake {
        key: String,
        reason: String,
        tail: String,
    },
    /// A Route's outbound literal failed its plugin step's startup `check`.
    #[error("{0}")]
    Check(String),
    #[error("plugin `{label}` {how}; Decoy stops without it{tail}")]
    Exited {
        label: String,
        how: String,
        tail: String,
    },
}

/// The running plugins, by key. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct Plugins {
    by_key: Arc<BTreeMap<String, Arc<Plugin>>>,
    host: Arc<ProtoHost>,
}

/// The same running plugins (a reload keeps them).
impl PartialEq for Plugins {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.by_key, &other.by_key)
    }
}

impl Eq for Plugins {}

impl Plugins {
    pub fn get(&self, key: &str) -> Option<&Arc<Plugin>> {
        self.by_key.get(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<Plugin>> {
        self.by_key.values()
    }

    /// The protos host calls use; a reload hands in the new ones.
    pub fn set_protos(&self, pool: DescriptorPool) {
        self.host.set_pool(pool);
    }

    /// While set, `protobuf.encode` host calls only check a value (startup `check`).
    pub fn set_checking(&self, checking: bool) {
        self.host.set_checking(checking);
    }
}

/// Watches the processes: reports the first one that exits, and stops them all on shutdown.
#[derive(Debug)]
pub struct Supervisor {
    exits: mpsc::UnboundedReceiver<PluginError>,
    stops: Vec<tokio::task::JoinHandle<()>>,
    plugins: Plugins,
}

impl Supervisor {
    /// Resolves when a plugin exits on its own; never, without plugins.
    pub async fn exited(&mut self) -> PluginError {
        match self.exits.recv().await {
            Some(error) => error,
            None => std::future::pending().await,
        }
    }

    /// Asks every plugin to exit, and kills those still running after [`SHUTDOWN_GRACE`].
    pub async fn shutdown(self) {
        for plugin in self.plugins.iter() {
            plugin.shutdown();
        }
        let aborts: Vec<_> = self.stops.iter().map(|stop| stop.abort_handle()).collect();
        let all = futures_util::future::join_all(self.stops);
        if tokio::time::timeout(SHUTDOWN_GRACE, all).await.is_err() {
            // Each watch owns its child; aborting it drops the child, which kills it.
            for abort in aborts {
                abort.abort();
            }
        }
    }
}

/// Starts every plugin in `configs` and waits for each to answer `ready`. On any failure the
/// ones already started are killed.
pub async fn start(
    configs: &BTreeMap<String, PluginConfig>,
    events: &crate::events::Sender,
) -> Result<(Plugins, Supervisor), PluginError> {
    let host = Arc::new(ProtoHost::default());
    let (exited, exits) = mpsc::unbounded_channel();
    let started = futures_util::future::join_all(
        configs
            .iter()
            .map(|(key, config)| spawn(key, config, host.clone(), events.clone(), exited.clone())),
    )
    .await;
    let mut by_key = BTreeMap::new();
    let mut stops = Vec::new();
    for started in started {
        // Dropping the rest kills them (`kill_on_drop`).
        let (plugin, stop) = started?;
        by_key.insert(plugin.key.clone(), plugin);
        stops.push(stop);
    }
    let plugins = Plugins {
        by_key: Arc::new(by_key),
        host,
    };
    Ok((
        plugins.clone(),
        Supervisor {
            exits,
            stops,
            plugins,
        },
    ))
}

async fn spawn(
    key: &str,
    config: &PluginConfig,
    host: Arc<ProtoHost>,
    events: crate::events::Sender,
    exited: mpsc::UnboundedSender<PluginError>,
) -> Result<(Arc<Plugin>, tokio::task::JoinHandle<()>), PluginError> {
    let mut child = shell(&config.command)
        .current_dir(&config.dir)
        .env("DECOY_PLUGIN", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| PluginError::Spawn {
            key: key.to_owned(),
            command: config.command.clone(),
            source,
        })?;
    let stderr = Arc::new(Stderr::default());
    let pipes = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    let (Some(stdin), Some(stdout), Some(stderr_pipe)) = pipes else {
        unreachable!("every pipe was asked for");
    };
    let forwarding = tokio::spawn(forward_stderr(
        key.to_owned(),
        stderr_pipe,
        stderr.clone(),
        events,
    ));
    let (handshake, reader) = connect(key, config.timeout, stdout, stdin, host, stderr.clone());
    let failed = |reason: String| PluginError::Handshake {
        key: key.to_owned(),
        reason,
        tail: String::new(),
    };
    let ready = tokio::select! {
        ready = handshake => ready.map_err(failed),
        status = child.wait() => Err(failed(match status {
            Ok(status) => format!("it exited ({status}) before answering `hello`"),
            Err(error) => error.to_string(),
        })),
        () = tokio::time::sleep(READY_TIMEOUT) => Err(failed(format!(
            "no `ready` within {}s",
            READY_TIMEOUT.as_secs()
        ))),
    };
    let plugin = match ready {
        Ok(plugin) => plugin,
        Err(PluginError::Handshake { key, reason, .. }) => {
            let _ = child.start_kill();
            // The last lines may still be on their way.
            let _ = tokio::time::timeout(Duration::from_millis(500), forwarding).await;
            return Err(PluginError::Handshake {
                key,
                reason,
                tail: stderr.tail(),
            });
        }
        Err(other) => return Err(other),
    };
    let label = plugin.label();
    // Owns the child: dropping this task (on shutdown) kills it.
    let watch = tokio::spawn(async move {
        let how = tokio::select! {
            status = child.wait() => exit_status(status),
            ended = reader => match ended {
                Ok(Err(reason)) => {
                    let _ = child.kill().await;
                    format!("broke the protocol: {reason}")
                }
                // stdout closed: the process is on its way out.
                _ => match tokio::time::timeout(SHUTDOWN_GRACE, child.wait()).await {
                    Ok(status) => exit_status(status),
                    Err(_) => {
                        let _ = child.kill().await;
                        "closed its stdout".to_owned()
                    }
                },
            },
        };
        let _ = tokio::time::timeout(Duration::from_millis(500), forwarding).await;
        let _ = exited.send(PluginError::Exited {
            label,
            how,
            tail: stderr.tail(),
        });
    });
    Ok((plugin, watch))
}

fn exit_status(status: std::io::Result<std::process::ExitStatus>) -> String {
    match status {
        Ok(status) => format!("exited ({status})"),
        Err(error) => format!("can't be watched: {error}"),
    }
}

#[cfg(unix)]
fn shell(command: &str) -> Command {
    let mut shell = Command::new("sh");
    shell.arg("-c").arg(command);
    shell
}

#[cfg(windows)]
fn shell(command: &str) -> Command {
    let mut shell = Command::new("cmd");
    shell.arg("/C").arg(command);
    shell
}
