//! The runtime server: binds the configured listeners and serves every transport.

mod ingress;
mod listen;
mod reload;
mod response;
mod ws;

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use crate::config::{ConfigError, ServeConfig, TlsSource};
use crate::engine::Controller;
use crate::http::RuntimeConfig;
use crate::protos::load_descriptor_pool;
use crate::schema::PassthroughTarget;
use crate::startup::{Startup, StartupError, load_catalog_from_files};
use crate::tls;
use axum::routing::any;
use axum::{Extension, Router};
use thiserror::Error;
use tokio::sync::RwLock;

use ingress::{MaxBodyBytes, Secure, handle_request};
use listen::bind;
pub use listen::{BindError, BoundListener};
use reload::ListenerFields;
pub use reload::{Reloaded, Reloader};

/// A started runtime: every listener is bound and serving until the shutdown signal fires.
pub struct Server {
    pub listeners: Vec<BoundListener>,
    /// Startup messages for the caller to show, e.g. where the auto TLS CA lives. `serve` prints
    /// nothing itself, not even the listen addresses.
    pub notices: Vec<String>,
    pub controller: Arc<RwLock<Controller>>,
    reloader: Reloader,
    events: crate::events::Sender,
    connections: crate::connections::Connections,
    workers: crate::workers::Supervisor,
    shutdown: Pin<Box<dyn Future<Output = ()> + Send>>,
    stop_all: tokio::sync::watch::Sender<()>,
    servers: tokio::task::JoinSet<std::io::Result<()>>,
}

/// Error returned by [`serve`], [`Server::wait`] and [`Reloader::reload`]. Each variant shows its
/// cause's text unchanged.
#[derive(Debug, Error)]
pub enum ServeError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Protos(StartupError),
    #[error(transparent)]
    Startup(#[from] StartupError),
    #[error(transparent)]
    Tls(StartupError),
    #[error(transparent)]
    Bind(#[from] BindError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Task(#[from] tokio::task::JoinError),
    #[error(transparent)]
    Worker(#[from] crate::workers::WorkerError),
}

/// Loads `config`, binds every listener and starts serving. `shutdown` is awaited by
/// [`Server::wait`]; when it resolves, open requests get [`DRAIN_TIMEOUT`] to finish.
pub async fn serve(
    config: ServeConfig,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<Server, ServeError> {
    config.validate()?;
    let events = crate::events::channel();
    // Before the Catalog: Routes name Transformer steps.
    let (workers, supervisor) = crate::workers::start(&config.workers, &events).await?;
    let startup = load(&config, &workers)?;
    workers.set_protos(startup.catalog.runtime().protos.clone());
    crate::pipeline::check_routes(&startup.catalog)
        .await
        .map_err(crate::workers::WorkerError::Check)?;
    let mut notices = Vec::new();
    let controller =
        Controller::new(startup.catalog, startup.default_collection).with_events(events.clone());
    let controller = Arc::new(RwLock::new(controller));
    let max_body_bytes = Arc::new(AtomicUsize::new(config.max_body_bytes));
    let connections = crate::connections::Connections::default();
    let app = Router::new()
        .fallback(any(handle_request))
        .with_state(controller.clone())
        .layer(Extension(MaxBodyBytes(max_body_bytes.clone())))
        .layer(Extension(connections.clone()))
        .layer(Extension(events.clone()));
    let tls_config = match &config.tls {
        None => None,
        Some(TlsSource::Files(files)) => Some(tls::server_config(files).map_err(ServeError::Tls)?),
        Some(TlsSource::Auto { dir }) => {
            let (tls_config, ca) = tls::auto_server_config(dir).map_err(ServeError::Tls)?;
            notices.push(format!(
                "TLS cert signed by {}; trust it with e.g. NODE_EXTRA_CA_CERTS",
                ca.display()
            ));
            Some(tls_config)
        }
    };
    // Bind every listener before serving any, so a taken port fails startup cleanly (Q1).
    let mut listeners = Vec::new();
    let named = std::iter::once(("port".to_owned(), &config.port)).chain(
        config
            .listeners
            .iter()
            .map(|(name, listener)| (format!("listeners.{name}"), listener)),
    );
    for (name, listener) in named {
        for bound in bind(&name, &listener.host, listener.port, &mut notices).await? {
            listeners.push((name.clone(), listener.tls, bound));
        }
    }

    let (stop_all, _) = tokio::sync::watch::channel(());
    let mut bound = Vec::new();
    let mut servers = tokio::task::JoinSet::new();
    for (name, secure, listener) in listeners {
        let addr = listener.local_addr()?;
        bound.push(BoundListener {
            name: name.clone(),
            tls: secure,
            addr,
        });
        let mut stop = stop_all.subscribe();
        match tls_config.clone().filter(|_| secure) {
            Some(tls_config) => {
                let app = app.clone().layer(Extension(Secure));
                servers.spawn(crate::events::logging_to(
                    events.clone(),
                    tls::serve(listener, app, tls_config, stop),
                ));
            }
            None => {
                servers.spawn(
                    axum::serve(listener, app.clone())
                        .with_graceful_shutdown(async move {
                            let _ = stop.changed().await;
                        })
                        .into_future(),
                );
            }
        }
    }
    let reloader = Reloader {
        controller: controller.clone(),
        max_body_bytes,
        listeners: Arc::new(ListenerFields::of(&config)),
        workers,
        worker_configs: Arc::new(config.workers.clone()),
        running: Arc::default(),
    };
    Ok(Server {
        listeners: bound,
        notices,
        controller,
        reloader,
        events,
        connections,
        workers: supervisor,
        shutdown: Box::pin(shutdown),
        stop_all,
        servers,
    })
}

/// Builds the Catalog from `config`'s protos, Routes and Collections.
fn load(config: &ServeConfig, workers: &crate::workers::Workers) -> Result<Startup, ServeError> {
    let protos = load_descriptor_pool(&config.protos).map_err(ServeError::Protos)?;
    Ok(load_catalog_from_files(
        &config.routes,
        &config.collections,
        RuntimeConfig {
            max_body_bytes: Some(config.max_body_bytes),
            passthrough: config
                .passthrough_base_url
                .clone()
                .map(|base_url| PassthroughTarget { base_url }),
            protos,
            pipelines: config
                .pipelines
                .iter()
                .map(|(name, spec)| (name.clone(), spec.expand(workers, &config.commands)))
                .collect(),
            commands: config.commands.clone(),
            workers: workers.clone(),
        },
        config.collection.as_deref(),
    )?)
}

impl Server {
    pub fn reloader(&self) -> Reloader {
        self.reloader.clone()
    }

    /// Every [`crate::events::Event`] from now on; see [`crate::events`].
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<crate::events::Event> {
        self.events.subscribe()
    }

    /// Serves until the shutdown signal fires or a listener fails, then drains.
    pub async fn wait(self) -> Result<(), ServeError> {
        let Self {
            events,
            connections,
            mut workers,
            shutdown,
            stop_all,
            mut servers,
            ..
        } = self;
        // A worker that exits stops Decoy (ADR 0004); its error is the result, after the drain.
        let mut stopped_by = None;
        tokio::select! {
            _ = shutdown => {}
            Some(result) = servers.join_next() => result??,
            error = workers.exited() => stopped_by = Some(error),
        }
        drop(stop_all);
        // Open streams would otherwise wait for `send` until the drain timeout.
        connections.close_all();
        // Let open requests finish, but never wait on a long `delay` or a hung upstream forever.
        let drained = tokio::time::timeout(DRAIN_TIMEOUT, async {
            while let Some(result) = servers.join_next().await {
                result??;
            }
            // Upgraded sockets run outside the servers; let them finish their close handshake.
            connections.all_ended().await;
            Ok::<_, ServeError>(())
        })
        .await;
        match drained {
            Ok(result) => result?,
            Err(_) => {
                crate::events::logging_to(events, async {
                    crate::events::warn(format!(
                        "closed connections still open after {}s",
                        DRAIN_TIMEOUT.as_secs()
                    ));
                })
                .await;
                servers.abort_all();
            }
        }
        workers.shutdown().await;
        match stopped_by {
            Some(error) => Err(error.into()),
            None => Ok(()),
        }
    }
}

pub const DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
