use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{ServeError, load};
use crate::config::{ServeConfig, TlsSource};
use crate::engine::{Catalog, Controller};
use tokio::sync::RwLock;

/// Applies a re-read config to a running server (`/reload`, `--watch`): everything but the
/// listeners, which stay bound as they are.
#[derive(Clone)]
pub struct Reloader {
    pub(super) controller: Arc<RwLock<Controller>>,
    pub(super) max_body_bytes: Arc<AtomicUsize>,
    pub(super) listeners: Arc<ListenerFields>,
    pub(super) workers: crate::workers::Workers,
    /// The Transformer workers started with; a reload doesn't restart them.
    pub(super) worker_configs: Arc<BTreeMap<String, crate::config::WorkerConfig>>,
    /// One reload at a time (`/reload` and `--watch`).
    pub(super) running: Arc<tokio::sync::Mutex<()>>,
}

/// What a successful reload applied.
#[derive(Debug, Clone)]
pub struct Reloaded {
    /// The Catalog now served.
    pub catalog: Arc<Catalog>,
    /// What a Session that never picked a Collection now uses.
    pub default_collection: String,
    /// `listener changes need restart`, `Transformer worker changes need restart`, then one line per Selection fallback or dropped override.
    pub warnings: Vec<String>,
}

impl Reloaded {
    /// `2 routes, 1 collection`.
    pub fn summary(&self) -> String {
        let count = |count: usize, noun: &str| {
            format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
        };
        format!(
            "{}, {}",
            count(self.catalog.routes().count(), "route"),
            count(self.catalog.collection_ids().count(), "collection")
        )
    }
}

/// The config fields a reload can't apply without rebinding.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ListenerFields {
    port: crate::config::Listener,
    listeners: BTreeMap<String, crate::config::Listener>,
    tls: Option<TlsSource>,
}

impl ListenerFields {
    pub(super) fn of(config: &ServeConfig) -> Self {
        Self {
            port: config.port.clone(),
            listeners: config.listeners.clone(),
            tls: config.tls.clone(),
        }
    }
}

impl Reloader {
    /// Reads the config with `read_config`, builds the new Catalog before taking the Controller's
    /// lock, then swaps it in. On an error nothing changes. Requests already running finish
    /// against the old Catalog. Reloads queue, and each one reads the files only once it runs,
    /// so the last one always applies the newest files.
    pub async fn reload(
        &self,
        read_config: impl FnOnce() -> Result<ServeConfig, ServeError> + Send + 'static,
    ) -> Result<Reloaded, ServeError> {
        let _running = self.running.lock().await;
        // File IO and the proto compile block, so keep them off the async workers.
        let workers = self.workers.clone();
        let (config, startup) = tokio::task::spawn_blocking(move || {
            let config = read_config()?;
            config.validate_transformer_names()?;
            let startup = load(&config, &workers)?;
            Ok::<_, ServeError>((config, startup))
        })
        .await??;
        let mut warnings = Vec::new();
        if ListenerFields::of(&config) != *self.listeners {
            warnings.push("listener changes need restart".to_owned());
        }
        if config.workers != *self.worker_configs {
            warnings.push("Transformer worker changes need restart".to_owned());
        }
        self.workers
            .set_protos(startup.catalog.runtime().protos.clone());
        let mut controller = self.controller.write().await;
        warnings.extend(controller.reload(startup.catalog, startup.default_collection));
        self.max_body_bytes
            .store(config.max_body_bytes, Ordering::Relaxed);
        Ok(Reloaded {
            catalog: controller.catalog(),
            default_collection: controller.default_collection().to_owned(),
            warnings,
        })
    }
}
