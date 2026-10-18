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
    KeyUsagePurpose, PublicKeyData,
};
use rustls::ServerConfig;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use time::{Duration as TimeDuration, OffsetDateTime};
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

/// `tls: auto`: signs a fresh localhost cert with the CA in `dir`, creating the CA key on first use
/// so a client that trusts `ca.pem` keeps trusting it across runs. Returns the config and CA path.
pub fn auto_server_config(dir: &Path) -> Result<(Arc<ServerConfig>, PathBuf), StartupError> {
    let ca_cert = dir.join("ca.pem");
    let ca_key = dir.join("ca-key.pem");
    let io_error = |path: &Path, error: std::io::Error| {
        StartupError::new(
            StartupDiagnosticKind::Io,
            path,
            format!("cannot set up the `tls: auto` CA: {error}"),
        )
    };
    fs::create_dir_all(dir).map_err(|error| io_error(dir, error))?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        // The CA key can sign any host; keep it out of the user's repo.
        fs::write(&ignore, "*\n").map_err(|error| io_error(&ignore, error))?;
    }
    let key = load_or_create_key(&ca_key).map_err(|error| io_error(&ca_key, error))?;
    let key = KeyPair::from_pem(&key).map_err(|error| {
        StartupError::new(
            StartupDiagnosticKind::Parse,
            &ca_key,
            format!("cannot read the `tls: auto` CA key: {error}; delete `.decoy/` to start over"),
        )
    })?;
    // ca.pem must match the key; rebuild it when it's missing or from another key. Same key and
    // subject, so clients that trusted an earlier copy still trust the certs it signs.
    let spki = key.subject_public_key_info();
    let current = fs::read(&ca_cert).ok().and_then(|pem| {
        CertificateDer::from_pem_slice(&pem)
            .ok()
            .filter(|der| contains(der, &spki))
    });
    if current.is_none() {
        let cert = ca_params()
            .self_signed(&key)
            .expect("the CA params are valid");
        publish(&ca_cert, cert.pem().as_bytes(), false)
            .map_err(|error| io_error(&ca_cert, error))?;
    }

    // An issuer rebuilt from the same params and key signs certs that chain to the stored CA.
    let issuer = Issuer::new(ca_params(), key);
    let params = leaf_params(OffsetDateTime::now_utc());
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

/// The localhost cert, in a shape strict verifiers (`openssl -x509_strict`, Apple, Python 3.13)
/// accept: an AKI naming the CA key, and a validity Apple allows (at most 825 days).
fn leaf_params(now: OffsetDateTime) -> CertificateParams {
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
    params.use_authority_key_identifier_extension = true;
    // A year outlasts any dev server between restarts, which re-sign it.
    params.not_before = now - TimeDuration::days(1);
    params.not_after = now + TimeDuration::days(365);
    params
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

/// Reads the CA key, or creates it. Two Decoys starting at once agree on one key: it's published
/// whole with a hard link that fails if another process won.
fn load_or_create_key(path: &Path) -> std::io::Result<String> {
    if let Ok(pem) = fs::read_to_string(path) {
        return Ok(pem);
    }
    let pem = KeyPair::generate()
        .expect("ring generates keys")
        .serialize_pem();
    match publish(path, pem.as_bytes(), true) {
        Ok(()) => Ok(pem),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => fs::read_to_string(path),
        Err(error) => Err(error),
    }
}

/// Writes `contents` to a temp file next to `path`, then moves it into place, so readers never see
/// a partial file. `exclusive` keeps an existing file (and makes the temp file private).
fn publish(path: &Path, contents: &[u8], exclusive: bool) -> std::io::Result<()> {
    let temp = path.with_extension(format!("tmp{}", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if exclusive {
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    }
    let written = options
        .open(&temp)
        .and_then(|mut file| std::io::Write::write_all(&mut file, contents));
    let moved = written.and_then(|()| {
        if exclusive {
            fs::hard_link(&temp, path)
        } else {
            fs::rename(&temp, path)
        }
    });
    let _ = fs::remove_file(&temp);
    moved
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
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
                        crate::events::error(format!("accept failed: {error}"));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaf_carries_an_aki_and_an_apple_safe_validity() {
        let params = leaf_params(OffsetDateTime::now_utc());
        assert!(params.not_after - params.not_before <= TimeDuration::days(825));

        let issuer = Issuer::new(ca_params(), KeyPair::generate().unwrap());
        let leaf = params
            .signed_by(&KeyPair::generate().unwrap(), &issuer)
            .unwrap();
        // id-ce-authorityKeyIdentifier (2.5.29.35)
        assert!(contains(leaf.der(), &[0x06, 0x03, 0x55, 0x1d, 0x23]));
    }
}
