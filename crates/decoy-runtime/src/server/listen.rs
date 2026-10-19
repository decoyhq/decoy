use std::net::SocketAddr;

use tokio::net::TcpListener;

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
) -> Result<Vec<TcpListener>, String> {
    let mut addrs: Vec<SocketAddr> = if host.eq_ignore_ascii_case("localhost") {
        vec![
            SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port)),
            SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port)),
        ]
    } else {
        tokio::net::lookup_host((host, port))
            .await
            .map_err(|error| format!("cannot resolve `{name}` host `{host}`: {error}"))?
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
                skipped = Some(format!("cannot listen on `{name}` ({addr}): {error}"));
            }
            Err(error) => return Err(format!("cannot listen on `{name}` ({addr}): {error}")),
        }
    }
    if bound.is_empty() {
        return Err(skipped.unwrap_or_else(|| format!("`{name}` host `{host}` has no addresses")));
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
}
