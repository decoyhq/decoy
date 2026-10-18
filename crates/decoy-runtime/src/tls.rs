use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
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

    build(certs, key).map_err(|error| {
        StartupError::new(
            StartupDiagnosticKind::Parse,
            &files.key,
            format!(
                "TLS key doesn't fit cert `{}`: {error}",
                files.cert.display()
            ),
        )
    })
}

/// `tls: auto`: signs a fresh localhost cert with the CA in `dir`, creating the CA on first use so
/// a client that trusts `ca.pem` keeps trusting it across runs. Returns the config and the CA path.
pub fn auto_server_config(dir: &Path) -> Result<(Arc<ServerConfig>, PathBuf), StartupError> {
    let ca_cert = dir.join("ca.pem");
    let ca_key = dir.join("ca-key.pem");
    let io_error = |path: &Path, error: std::io::Error| {
        StartupError::new(StartupDiagnosticKind::Io, path, error.to_string())
    };
    let key = if ca_key.exists() {
        let pem = fs::read_to_string(&ca_key).map_err(|error| io_error(&ca_key, error))?;
        KeyPair::from_pem(&pem).map_err(|error| {
            StartupError::new(
                StartupDiagnosticKind::Parse,
                &ca_key,
                format!("cannot read generated CA key: {error}; delete it to start over"),
            )
        })?
    } else {
        let key = KeyPair::generate().expect("ring generates keys");
        fs::create_dir_all(dir).map_err(|error| io_error(dir, error))?;
        write_private(&ca_key, &key.serialize_pem()).map_err(|error| io_error(&ca_key, error))?;
        key
    };
    if !ca_cert.exists() {
        let cert = ca_params()
            .self_signed(&key)
            .expect("the CA params are valid");
        fs::write(&ca_cert, cert.pem()).map_err(|error| io_error(&ca_cert, error))?;
    }
    // An issuer rebuilt from the same params and key signs certs that chain to the stored CA.
    let issuer = Issuer::new(ca_params(), key);
    let mut params = CertificateParams::new(vec![
        "localhost".to_owned(),
        "127.0.0.1".to_owned(),
        "::1".to_owned(),
    ])
    .expect("the localhost names are valid");
    params
        .distinguished_name
        .push(DnType::CommonName, "localhost");
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let leaf_key = KeyPair::generate().expect("ring generates keys");
    let leaf = params
        .signed_by(&leaf_key, &issuer)
        .expect("the CA signs the localhost cert");
    let config = build(
        vec![leaf.der().clone()],
        PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into()),
    )
    .expect("the generated cert and key fit");
    Ok((config, ca_cert))
}

fn ca_params() -> CertificateParams {
    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, "Decoy local CA");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params
}

fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(path)?, contents.as_bytes())
}

fn build(
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<Arc<ServerConfig>, rustls::Error> {
    let mut config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("ring supports the default TLS versions")
            .with_no_client_auth()
            .with_single_cert(certs, key)?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

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
                Err(error) => {
                    // Like `axum::serve`: skip per-connection failures, back off on the rest (e.g. EMFILE).
                    if !matches!(
                        error.kind(),
                        ErrorKind::ConnectionRefused
                            | ErrorKind::ConnectionAborted
                            | ErrorKind::ConnectionReset
                    ) {
                        eprintln!("decoy serve: accept failed: {error}");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    continue;
                }
            },
            _ = stop.changed() => break,
        };
        let acceptor = acceptor.clone();
        let service = TowerToHyperService::new(app.clone());
        let watcher = graceful.watcher();
        let mut handshake_stop = stop.clone();
        tokio::spawn(async move {
            // A failed, slow, or silent handshake only affects that client and never holds up
            // shutdown.
            let stream = tokio::select! {
                accepted = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)) => {
                    match accepted {
                        Ok(Ok(stream)) => stream,
                        _ => return,
                    }
                }
                _ = handshake_stop.changed() => return,
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
