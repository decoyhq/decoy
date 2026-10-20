//! Live long-lived connections (WebSocket, SSE, streaming gRPC), so the Control API `send` can
//! push messages to them (`docs/design/transports.md` §9).
//!
//! A transport registers each connection with the Session it opened with (§10) and reads the
//! messages pushed to it from the returned receiver. `send` encodes each message through the Route's
//! Pipeline once, against the current Catalog; the registry carries those bytes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{Notify, mpsc};

#[derive(Debug, Clone, Default)]
pub struct Connections {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    live: Mutex<Live>,
    /// Wakes `send` calls waiting for a connection.
    registered: Notify,
}

#[derive(Debug, Default)]
struct Live {
    /// Set by [`Connections::close_all`]: later registrations are closed at once.
    closed: bool,
    next_id: u64,
    /// Each connection with the full name of the output type it opened with.
    by_address: HashMap<Address, Vec<(u64, String, Sender)>>,
}

/// Session + Route id.
type Address = (String, String);

/// Carries encoded messages to one connection.
type Sender = mpsc::UnboundedSender<Vec<u8>>;

/// Keeps a connection registered; dropping it unregisters the connection.
#[derive(Debug)]
pub struct Registration {
    inner: Arc<Inner>,
    address: Address,
    id: u64,
}

impl Connections {
    /// Registers a live connection of `route` in `session` that writes `output` messages (the
    /// type's full name). Messages pushed by [`Self::send`] arrive on the receiver until the
    /// [`Registration`] drops.
    pub fn register(
        &self,
        session: &str,
        route: &str,
        output: &str,
    ) -> (Registration, mpsc::UnboundedReceiver<Vec<u8>>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let address = (session.to_owned(), route.to_owned());
        let mut live = self.inner.live.lock().expect("connections lock");
        let id = live.next_id;
        live.next_id += 1;
        if !live.closed {
            live.by_address.entry(address.clone()).or_default().push((
                id,
                output.to_owned(),
                sender,
            ));
        }
        drop(live);
        self.inner.registered.notify_waiters();
        let registration = Registration {
            inner: self.inner.clone(),
            address,
            id,
        };
        (registration, receiver)
    }

    /// Delivers `message`, encoded as `output`, to every live connection of `route` in `session`
    /// opened with that output type; one opened before a reload changed it is skipped. With none,
    /// waits up to `timeout` for one (Q5). Returns how many connections got it.
    pub async fn send(
        &self,
        session: &str,
        route: &str,
        output: &str,
        message: Vec<u8>,
        timeout: Duration,
    ) -> Result<usize, SendError> {
        let address = (session.to_owned(), route.to_owned());
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Listen before checking, so a registration between the check and the wait still wakes us.
            let registered = self.inner.registered.notified();
            let delivered = self.deliver(&address, output, &message)?;
            if delivered > 0 {
                return Ok(delivered);
            }
            if tokio::time::timeout_at(deadline, registered).await.is_err() {
                return Err(SendError::Timeout);
            }
        }
    }

    /// Ends every live connection: their receivers yield `None`. Used on shutdown, so open
    /// streams finish instead of holding the drain.
    pub fn close_all(&self) {
        let mut live = self.inner.live.lock().expect("connections lock");
        live.closed = true;
        live.by_address.clear();
        drop(live);
        // Waiting `send`s stop too, so they don't hold the drain either.
        self.inner.registered.notify_waiters();
    }

    fn deliver(&self, address: &Address, output: &str, message: &[u8]) -> Result<usize, SendError> {
        let live = self.inner.live.lock().expect("connections lock");
        if live.closed {
            return Err(SendError::Closed);
        }
        Ok(live.by_address.get(address).map_or(0, |senders| {
            senders
                .iter()
                .filter(|(_, opened, sender)| {
                    opened == output && sender.send(message.to_vec()).is_ok()
                })
                .count()
        }))
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let mut live = self.inner.live.lock().expect("connections lock");
        if let Some(senders) = live.by_address.get_mut(&self.address) {
            senders.retain(|(id, _, _)| *id != self.id);
            if senders.is_empty() {
                live.by_address.remove(&self.address);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    /// No live connection appeared before the `send` timeout.
    #[error("no live connection before the timeout")]
    Timeout,
    /// Decoy is shutting down; no connection will come.
    #[error("decoy is shutting down")]
    Closed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn close_all_stops_a_waiting_send() {
        let connections = Connections::default();
        let sending = tokio::spawn({
            let connections = connections.clone();
            async move {
                connections
                    .send("a", "live", "live.Tick", vec![1], Duration::from_secs(30))
                    .await
            }
        });
        tokio::task::yield_now().await;
        connections.close_all();

        let sent = tokio::time::timeout(Duration::from_secs(1), sending).await;
        assert_eq!(sent.unwrap().unwrap(), Err(SendError::Closed));
    }

    const SHORT: Duration = Duration::from_millis(20);

    #[tokio::test]
    async fn send_reaches_every_connection_of_the_route_in_that_session_only() {
        let connections = Connections::default();
        let (_a1, mut a1) = connections.register("a", "live", "live.Tick");
        let (_a2, mut a2) = connections.register("a", "live", "live.Tick");
        let (_b, mut b) = connections.register("b", "live", "live.Tick");
        let (_other, mut other) = connections.register("a", "other", "live.Tick");

        let delivered = connections
            .send("a", "live", "live.Tick", vec![1], SHORT)
            .await;

        assert_eq!(delivered, Ok(2));
        assert_eq!(a1.try_recv().unwrap(), vec![1]);
        assert_eq!(a2.try_recv().unwrap(), vec![1]);
        assert!(b.try_recv().is_err());
        assert!(other.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_dropped_registration_no_longer_receives() {
        let connections = Connections::default();
        let (kept, mut kept_rx) = connections.register("a", "live", "live.Tick");
        let (gone, _gone_rx) = connections.register("a", "live", "live.Tick");
        drop(gone);

        assert_eq!(
            connections
                .send("a", "live", "live.Tick", vec![1], SHORT)
                .await,
            Ok(1)
        );
        assert_eq!(kept_rx.try_recv().unwrap(), vec![1]);

        drop(kept);
        assert_eq!(
            connections
                .send("a", "live", "live.Tick", vec![2], SHORT)
                .await,
            Err(SendError::Timeout)
        );
    }

    #[tokio::test]
    async fn close_all_ends_every_connection_and_later_ones() {
        let connections = Connections::default();
        let (_open, mut open) = connections.register("a", "live", "live.Tick");
        connections.close_all();
        let (_late, mut late) = connections.register("a", "live", "live.Tick");

        assert_eq!(open.recv().await, None);
        assert_eq!(late.recv().await, None);
    }

    #[tokio::test]
    async fn send_waits_for_a_connection_then_delivers() {
        let connections = Connections::default();
        let sending = tokio::spawn({
            let connections = connections.clone();
            async move {
                connections
                    .send("a", "live", "live.Tick", vec![9], Duration::from_secs(5))
                    .await
            }
        });
        tokio::task::yield_now().await;
        let (_registration, mut receiver) = connections.register("a", "live", "live.Tick");

        assert_eq!(sending.await.unwrap(), Ok(1));
        assert_eq!(receiver.recv().await.unwrap(), vec![9]);
    }

    #[tokio::test]
    async fn send_skips_connections_opened_with_another_output_type() {
        let connections = Connections::default();
        let (_old, mut old) = connections.register("a", "live", "live.Old");

        assert_eq!(
            connections
                .send("a", "live", "live.New", vec![1], SHORT)
                .await,
            Err(SendError::Timeout)
        );
        assert!(old.try_recv().is_err());
    }

    #[tokio::test]
    async fn send_times_out_without_a_connection_in_that_session() {
        let connections = Connections::default();
        let (_elsewhere, _receiver) = connections.register("b", "live", "live.Tick");

        assert_eq!(
            connections
                .send("a", "live", "live.Tick", vec![1], SHORT)
                .await,
            Err(SendError::Timeout)
        );
    }
}
