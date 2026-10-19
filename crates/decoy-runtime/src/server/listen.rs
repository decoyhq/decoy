use std::net::SocketAddr;

use thiserror::Error;
use tokio::net::TcpListener;

/// Why a listener didn't bind.
#[derive(Debug, Error)]
pub enum BindError {
    #[error("cannot resolve `{name}` host `{host}`: {error}")]
    Resolve {
        name: String,
        host: String,
        #[source]
        error: std::io::Error,
    },
    #[error("cannot listen on `{name}` ({addr}): {error}")]
    Listen {
        name: String,
        addr: SocketAddr,
        #[source]
        error: std::io::Error,
    },
    #[error("`{name}` host `{host}` has no addresses")]
    NoAddresses { name: String, host: String },
}

/// One bound socket: the config key it serves (`port` or `listeners.<name>`), TLS, address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundListener {
    pub name: String,
    pub tls: bool,
    pub addr: SocketAddr,
}

impl BoundListener {
    pub fn url(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        format!("{scheme}://{}", self.addr)
    }

    /// The URL, plus the config key unless it is the main `port`: what `listening on` shows.
    pub fn label(&self) -> String {
        match self.name.as_str() {
            "port" => self.url(),
            name => format!("{} ({name})", self.url()),
        }
    }
}

/// Binds every address `host` resolves to on one port. `localhost` is both loopbacks (`127.0.0.1`,
/// `::1`) without asking the resolver, which often maps it to `127.0.0.1` only. An address the
/// machine can't bind (e.g. `::1` with IPv6 off) is skipped while another one binds; a taken port
/// always fails; a skipped address adds a notice.
pub(super) async fn bind(
    name: &str,
    host: &str,
    port: u16,
    notices: &mut Vec<String>,
) -> Result<Vec<TcpListener>, BindError> {
    let mut addrs: Vec<SocketAddr> = if host.eq_ignore_ascii_case("localhost") {
        vec![
            SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port)),
            SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port)),
        ]
    } else {
        tokio::net::lookup_host((host, port))
            .await
            .map_err(|error| BindError::Resolve {
                name: name.to_owned(),
                host: host.to_owned(),
                error,
            })?
            .collect()
    };
    addrs.sort();
    addrs.dedup();
    let mut bound = Vec::new();
    let mut skipped = None;
    for mut addr in addrs {
        // Port 0: the first address picks a free port; the others share it.
        if let Some(first) = bound
            .first()
            .and_then(|first: &TcpListener| first.local_addr().ok())
        {
            addr.set_port(first.port());
        }
        match TcpListener::bind(addr).await {
            Ok(listener) => bound.push(listener),
            Err(error)
                if !matches!(
                    error.kind(),
                    std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                notices.push(format!("skipped `{name}` on {addr}: {error}"));
                skipped = Some(BindError::Listen {
                    name: name.to_owned(),
                    addr,
                    error,
                });
            }
            Err(error) => {
                return Err(BindError::Listen {
                    name: name.to_owned(),
                    addr,
                    error,
                });
            }
        }
    }
    if bound.is_empty() {
        return Err(skipped.unwrap_or_else(|| BindError::NoAddresses {
            name: name.to_owned(),
            host: host.to_owned(),
        }));
    }
    Ok(bound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn port_zero_picks_one_port_for_every_address_of_a_host() {
        let bound = bind("t", "localhost", 0, &mut Vec::new()).await.unwrap();
        assert!(!bound.is_empty());
        let ports: Vec<u16> = bound
            .iter()
            .map(|listener| listener.local_addr().unwrap().port())
            .collect();
        assert!(ports.iter().all(|port| *port == ports[0]), "{ports:?}");
    }

    #[tokio::test]
    async fn a_taken_port_fails_with_the_listener_and_address() {
        let taken = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = taken.local_addr().unwrap();
        let error = bind("t", "127.0.0.1", addr.port(), &mut Vec::new())
            .await
            .unwrap_err();
        assert!(matches!(error, BindError::Listen { .. }), "{error:?}");
        assert!(
            error
                .to_string()
                .starts_with(&format!("cannot listen on `t` ({addr}): ")),
            "{error}"
        );
        let source = std::error::Error::source(&error).expect("the I/O error is the source");
        assert!(source.is::<std::io::Error>(), "{source:?}");
    }
}
