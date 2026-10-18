use std::path::Path;
use std::sync::Arc;

use axum::Router;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use rustls::ServerConfig;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;

use crate::config::TlsFiles;
use crate::startup::{StartupDiagnosticKind, StartupError};

/// Builds the server config for TLS listeners; ALPN lets one port speak gRPC (h2) and HTTP/1.1.
pub fn server_config(files: &TlsFiles) -> Result<Arc<ServerConfig>, StartupError> {
    let read_error = |what: &str, path: &Path, error: rustls::pki_types::pem::Error| {
        StartupError::new(
            StartupDiagnosticKind::Io,
            path,
            format!("cannot read TLS {what}: {error}"),
        )
    };
    let certs = CertificateDer::pem_file_iter(&files.cert)
        .and_then(|certs| certs.collect::<Result<Vec<_>, _>>())
        .map_err(|error| read_error("cert", &files.cert, error))?;
    if certs.is_empty() {
        return Err(StartupError::new(
            StartupDiagnosticKind::Parse,
            &files.cert,
            "TLS cert holds no PEM certificates",
        ));
    }
    let key = PrivateKeyDer::from_pem_file(&files.key)
        .map_err(|error| read_error("key", &files.key, error))?;

    let mut config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("ring supports the default TLS versions")
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|error| {
                StartupError::new(
                    StartupDiagnosticKind::Parse,
                    &files.key,
                    format!(
                        "TLS key doesn't fit cert `{}`: {error}",
                        files.cert.display()
                    ),
                )
            })?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// Serves `app` over TLS until `stop` fires, then drains open connections.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    config: Arc<ServerConfig>,
    mut stop: watch::Receiver<()>,
) -> std::io::Result<()> {
    let acceptor = TlsAcceptor::from(config);
    let graceful = GracefulShutdown::new();
    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(_) => continue,
            },
            _ = stop.changed() => break,
        };
        let acceptor = acceptor.clone();
        let service = TowerToHyperService::new(app.clone());
        let watcher = graceful.watcher();
        tokio::spawn(async move {
            // A failed handshake only affects that client.
            let Ok(stream) = acceptor.accept(stream).await else {
                return;
            };
            let connection = auto::Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(TokioIo::new(stream), service)
                .into_owned();
            let _ = watcher.watch(connection).await;
        });
    }
    graceful.shutdown().await;
    Ok(())
}
