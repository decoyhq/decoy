//! Live long-lived connections (WebSocket, SSE, streaming gRPC), so the Control API `send` can
//! push messages to them (`docs/design/transports.md` §9).
//!
//! A transport registers each connection with the Session it opened with (§10) and reads the
//! messages pushed to it from the returned receiver. Encoding through the Route's pipeline is the
//! transport's job; the registry only carries the message JSON.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
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
    next_id: u64,
    by_address: HashMap<Address, Vec<(u64, mpsc::UnboundedSender<Value>)>>,
}

/// Session + Route id.
type Address = (String, String);

/// Keeps a connection registered; dropping it unregisters the connection.
#[derive(Debug)]
pub struct Registration {
    inner: Arc<Inner>,
    address: Address,
    id: u64,
}

impl Connections {
    /// Registers a live connection of `route` in `session`. Messages pushed by [`Self::send`]
    /// arrive on the receiver until the [`Registration`] drops.
    pub fn register(
        &self,
        session: &str,
        route: &str,
    ) -> (Registration, mpsc::UnboundedReceiver<Value>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let address = (session.to_owned(), route.to_owned());
        let mut live = self.inner.live.lock().expect("connections lock");
        let id = live.next_id;
        live.next_id += 1;
        live.by_address
            .entry(address.clone())
            .or_default()
            .push((id, sender));
        drop(live);
        self.inner.registered.notify_waiters();
        let registration = Registration {
            inner: self.inner.clone(),
            address,
            id,
        };
        (registration, receiver)
    }

    /// Delivers `message` to every live connection of `route` in `session`. With none, waits up
    /// to `timeout` for one (Q5). Returns how many connections got it.
    pub async fn send(
        &self,
        session: &str,
        route: &str,
        message: Value,
        timeout: Duration,
    ) -> Result<usize, SendError> {
        let address = (session.to_owned(), route.to_owned());
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Listen before checking, so a registration between the check and the wait still wakes us.
            let registered = self.inner.registered.notified();
            let delivered = self.deliver(&address, &message);
            if delivered > 0 {
                return Ok(delivered);
            }
            if tokio::time::timeout_at(deadline, registered).await.is_err() {
                return Err(SendError::Timeout);
            }
        }
    }

    fn deliver(&self, address: &Address, message: &Value) -> usize {
        let live = self.inner.live.lock().expect("connections lock");
        live.by_address.get(address).map_or(0, |senders| {
            senders
                .iter()
                .filter(|(_, sender)| sender.send(message.clone()).is_ok())
                .count()
        })
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let mut live = self.inner.live.lock().expect("connections lock");
        if let Some(senders) = live.by_address.get_mut(&self.address) {
            senders.retain(|(id, _)| *id != self.id);
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
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const SHORT: Duration = Duration::from_millis(20);

    #[tokio::test]
    async fn send_reaches_every_connection_of_the_route_in_that_session_only() {
        let connections = Connections::default();
        let (_a1, mut a1) = connections.register("a", "live");
        let (_a2, mut a2) = connections.register("a", "live");
        let (_b, mut b) = connections.register("b", "live");
        let (_other, mut other) = connections.register("a", "other");

        let delivered = connections
            .send("a", "live", json!({ "n": 1 }), SHORT)
            .await;

        assert_eq!(delivered, Ok(2));
        assert_eq!(a1.try_recv().unwrap(), json!({ "n": 1 }));
        assert_eq!(a2.try_recv().unwrap(), json!({ "n": 1 }));
        assert!(b.try_recv().is_err());
        assert!(other.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_dropped_registration_no_longer_receives() {
        let connections = Connections::default();
        let (kept, mut kept_rx) = connections.register("a", "live");
        let (gone, _gone_rx) = connections.register("a", "live");
        drop(gone);

        assert_eq!(connections.send("a", "live", json!(1), SHORT).await, Ok(1));
        assert_eq!(kept_rx.try_recv().unwrap(), json!(1));

        drop(kept);
        assert_eq!(
            connections.send("a", "live", json!(2), SHORT).await,
            Err(SendError::Timeout)
        );
    }

    #[tokio::test]
    async fn send_waits_for_a_connection_then_delivers() {
        let connections = Connections::default();
        let sending = tokio::spawn({
            let connections = connections.clone();
            async move {
                connections
                    .send("a", "live", json!("hi"), Duration::from_secs(5))
                    .await
            }
        });
        tokio::task::yield_now().await;
        let (_registration, mut receiver) = connections.register("a", "live");

        assert_eq!(sending.await.unwrap(), Ok(1));
        assert_eq!(receiver.recv().await.unwrap(), json!("hi"));
    }

    #[tokio::test]
    async fn send_times_out_without_a_connection_in_that_session() {
        let connections = Connections::default();
        let (_elsewhere, _receiver) = connections.register("b", "live");

        assert_eq!(
            connections.send("a", "live", json!(1), SHORT).await,
            Err(SendError::Timeout)
        );
    }
}
