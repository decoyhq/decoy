//! Worker processes: started with Decoy, one per `transformers` worker, spoken to over stdin/stdout
//! (`apps/docs/content/en/docs/transformer-worker-protocol/index.md`). A worker exiting stops Decoy; nothing restarts it
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
use tokio::task::JoinSet;

use crate::config::WorkerConfig;
pub use host::{Call, CallConnection, CallError, CallRequest, Frame, Op, Worker};
use host::{Stderr, connect, forward_stderr};
use protobuf::ProtoHost;

/// How long a worker may take to answer `hello`.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a worker has to exit after `shutdown` before it is killed.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Why workers didn't start, or why one stopped.
#[derive(Debug, Error)]
pub enum WorkerError {
    #[error("Transformer worker `{key}`: can't run `{command}`: {source}")]
    Spawn {
        key: String,
        command: String,
        source: std::io::Error,
    },
    #[error("Transformer worker `{key}` didn't start: {reason}{tail}")]
    Handshake {
        key: String,
        reason: String,
        tail: String,
    },
    /// A Route's outbound literal failed its Transformer step's startup `check`.
    #[error("{0}")]
    Check(String),
    #[error("Transformer worker `{label}` {how}; Decoy stops without it{tail}")]
    Exited {
        label: String,
        how: String,
        tail: String,
    },
}

/// The running workers, by key. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct Workers {
    by_key: Arc<BTreeMap<String, Arc<Worker>>>,
    host: Arc<ProtoHost>,
}

/// The same running workers (a reload keeps them).
impl PartialEq for Workers {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.by_key, &other.by_key)
    }
}

impl Eq for Workers {}

impl Workers {
    pub fn get(&self, key: &str) -> Option<&Arc<Worker>> {
        self.by_key.get(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<Worker>> {
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
    exits: mpsc::UnboundedReceiver<WorkerError>,
    stops: JoinSet<()>,
    workers: Workers,
}

impl Supervisor {
    /// Resolves when a worker exits on its own; never, without workers.
    pub async fn exited(&mut self) -> WorkerError {
        match self.exits.recv().await {
            Some(error) => error,
            None => std::future::pending().await,
        }
    }

    /// Asks every worker to exit, and kills those still running after [`SHUTDOWN_GRACE`].
    pub async fn shutdown(mut self) {
        for worker in self.workers.iter() {
            worker.shutdown();
        }
        let wait = async { while self.stops.join_next().await.is_some() {} };
        if tokio::time::timeout(SHUTDOWN_GRACE, wait).await.is_err() {
            self.stops.shutdown().await;
        }
    }
}

/// Starts every worker in `configs` and waits for each to answer `ready`. On any failure the
/// ones already started are killed.
pub async fn start(
    configs: &BTreeMap<String, WorkerConfig>,
    events: &crate::events::Sender,
) -> Result<(Workers, Supervisor), WorkerError> {
    let host = Arc::new(ProtoHost::default());
    let (exited, exits) = mpsc::unbounded_channel();
    let started = futures_util::future::join_all(
        configs
            .iter()
            .map(|(key, config)| spawn(key, config, host.clone(), events.clone(), exited.clone())),
    )
    .await;
    let mut by_key = BTreeMap::new();
    let mut stops = JoinSet::new();
    for started in started {
        // Dropping the rest kills them (`kill_on_drop`).
        let (worker, stop) = started?;
        by_key.insert(worker.key.clone(), worker);
        stops.spawn(stop);
    }
    let workers = Workers {
        by_key: Arc::new(by_key),
        host,
    };
    Ok((
        workers.clone(),
        Supervisor {
            exits,
            stops,
            workers,
        },
    ))
}

async fn spawn(
    key: &str,
    config: &WorkerConfig,
    host: Arc<ProtoHost>,
    events: crate::events::Sender,
    exited: mpsc::UnboundedSender<WorkerError>,
) -> Result<(Arc<Worker>, impl Future<Output = ()> + use<>), WorkerError> {
    let mut child = Command::new(&config.command[0])
        .args(&config.command[1..])
        .current_dir(&config.dir)
        .env("DECOY_TRANSFORMER", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| WorkerError::Spawn {
            key: key.to_owned(),
            command: config.command.join(" "),
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
    let failed = |reason: String| WorkerError::Handshake {
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
    let worker = match ready {
        Ok(worker) => worker,
        Err(WorkerError::Handshake { key, reason, .. }) => {
            let _ = child.start_kill();
            // The last lines may still be on their way.
            let _ = tokio::time::timeout(Duration::from_millis(500), forwarding).await;
            return Err(WorkerError::Handshake {
                key,
                reason,
                tail: stderr.tail(),
            });
        }
        Err(other) => return Err(other),
    };
    let label = worker.label();
    // Owns the child: dropping this task (on shutdown) kills it.
    let watch = async move {
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
        let _ = exited.send(WorkerError::Exited {
            label,
            how,
            tail: stderr.tail(),
        });
    };
    Ok((worker, watch))
}

fn exit_status(status: std::io::Result<std::process::ExitStatus>) -> String {
    match status {
        Ok(status) => format!("exited ({status})"),
        Err(error) => format!("can't be watched: {error}"),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failing_startup_stops_workers_that_already_answered_ready() {
        let dir = tempfile::tempdir().unwrap();
        let ready = wire::message(
            "ready",
            [
                ("protocol", 1.into()),
                ("name", "test".into()),
                ("version", "1".into()),
                ("directions", rmpv::Value::Array(vec!["input".into()])),
            ],
        );
        let mut bytes = Vec::new();
        wire::write(&mut bytes, &ready).await.unwrap();
        std::fs::write(dir.path().join("ready.bin"), bytes).unwrap();

        let worker = |script: &str| WorkerConfig {
            command: vec!["sh".into(), "-c".into(), script.into()],
            dir: dir.path().to_path_buf(),
            timeout: Duration::from_secs(1),
        };
        let configs = BTreeMap::from([
            (
                "broken".to_owned(),
                worker("while [ ! -f ready ]; do sleep 0.01; done; exit 1"),
            ),
            (
                "healthy".to_owned(),
                worker("cat ready.bin; touch ready; sleep 0.3; touch survived"),
            ),
        ]);

        assert!(start(&configs, &crate::events::channel()).await.is_err());
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(!dir.path().join("survived").exists());
    }
}
