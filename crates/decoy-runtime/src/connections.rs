//! Live long-lived connections (WebSocket, SSE, streaming gRPC), so the Control API `send` can
//! push messages to them and `wait` can wait for one (`docs/design/transports.md` §9).
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
    /// Wakes [`Connections::all_ended`] as connections end.
    ended: Notify,
}

#[derive(Debug, Default)]
struct Live {
    /// Set by [`Connections::close_all`]: later registrations are closed at once.
    closed: bool,
    next_id: u64,
    /// Registrations not yet dropped, including those `close_all` already ended.
    open: usize,
    /// Each connection with the full name of the output type it opened with.
    by_address: HashMap<Address, Vec<(u64, String, Sender)>>,
}

/// Session + Route id.
type Address = (String, String);

/// Carries encoded messages to one connection.
type Sender = mpsc::Sender<Vec<u8>>;

/// Messages a connection may have queued and unread; past it, `send` skips that connection.
const SEND_QUEUE: usize = 64;

/// How often a waiting `send` looks again for a connection with room.
const FULL_QUEUE_RETRY: Duration = Duration::from_millis(10);

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
    ) -> (Registration, mpsc::Receiver<Vec<u8>>) {
        let (sender, receiver) = mpsc::channel(SEND_QUEUE);
        let address = (session.to_owned(), route.to_owned());
        let mut live = self.inner.live.lock().expect("connections lock");
        let id = live.next_id;
        live.next_id += 1;
        live.open += 1;
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
    /// waits up to `timeout` for one (Q5). Returns how many connections got it; a connection whose
    /// queue is full does not.
    pub async fn send(
        &self,
        session: &str,
        route: &str,
        output: &str,
        message: Vec<u8>,
        timeout: Duration,
    ) -> Result<usize, SendError> {
        let address = (session.to_owned(), route.to_owned());
        self.wait_for(timeout, || self.deliver(&address, output, &message))
            .await
    }

    /// Waits up to `timeout` until `session` has a live connection of `route`, whatever output
    /// type it opened with. Returns how many it has.
    pub async fn wait_connected(
        &self,
        session: &str,
        route: &str,
        timeout: Duration,
    ) -> Result<usize, SendError> {
        let address = (session.to_owned(), route.to_owned());
        self.wait_for(timeout, || {
            let live = self.inner.live.lock().expect("connections lock");
            if live.closed {
                return Err(SendError::Closed);
            }
            Ok(live.by_address.get(&address).map_or(0, Vec::len))
        })
        .await
    }

    /// Runs `check` until it counts at least one connection, waking on each registration.
    async fn wait_for(
        &self,
        timeout: Duration,
        check: impl Fn() -> Result<usize, SendError>,
    ) -> Result<usize, SendError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Listen before checking, so a registration between the check and the wait still wakes us.
            let registered = self.inner.registered.notified();
            let found = check()?;
            if found > 0 {
                return Ok(found);
            }
            // A full queue frees up without a registration, so look again soon.
            let retry = deadline.min(tokio::time::Instant::now() + FULL_QUEUE_RETRY);
            let _ = tokio::time::timeout_at(retry, registered).await;
            if tokio::time::Instant::now() >= deadline {
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

    /// Resolves once every registered connection has ended, so shutdown can wait for sockets
    /// to finish their close handshake after [`Self::close_all`].
    pub async fn all_ended(&self) {
        loop {
            let ended = self.inner.ended.notified();
            if self.inner.live.lock().expect("connections lock").open == 0 {
                return;
            }
            ended.await;
        }
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
                    opened == output && sender.try_send(message.to_vec()).is_ok()
                })
                .count()
        }))
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let mut live = self.inner.live.lock().expect("connections lock");
        live.open -= 1;
        if let Some(senders) = live.by_address.get_mut(&self.address) {
            senders.retain(|(id, _, _)| *id != self.id);
            if senders.is_empty() {
                live.by_address.remove(&self.address);
            }
        }
        drop(live);
        self.inner.ended.notify_waiters();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    /// No live connection with room in its queue appeared before the `send` timeout.
    #[error("no live connection took the message before the timeout")]
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
    async fn all_ended_waits_for_every_registration_to_drop() {
        let connections = Connections::default();
        let (open, _receiver) = connections.register("a", "live", "live.Tick");
        connections.close_all();
        let waiting = tokio::spawn({
            let connections = connections.clone();
            async move { connections.all_ended().await }
        });
        tokio::time::sleep(SHORT).await;
        assert!(!waiting.is_finished());

        drop(open);
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap();
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
    async fn a_connection_with_a_full_queue_does_not_count_as_delivered() {
        let connections = Connections::default();
        let (_slow, mut slow) = connections.register("a", "live", "live.Tick");
        let send = |message| connections.send("a", "live", "live.Tick", message, SHORT);
        for _ in 0..SEND_QUEUE {
            assert_eq!(send(vec![1]).await, Ok(1));
        }

        assert_eq!(send(vec![2]).await, Err(SendError::Timeout));
        slow.recv().await.unwrap();
        assert_eq!(send(vec![3]).await, Ok(1));
    }

    #[tokio::test]
    async fn send_waits_for_a_full_queue_to_drain() {
        let connections = Connections::default();
        let (_slow, mut slow) = connections.register("a", "live", "live.Tick");
        for _ in 0..SEND_QUEUE {
            connections
                .send("a", "live", "live.Tick", vec![1], SHORT)
                .await
                .unwrap();
        }
        let sending = tokio::spawn({
            let connections = connections.clone();
            async move {
                connections
                    .send("a", "live", "live.Tick", vec![2], Duration::from_secs(5))
                    .await
            }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        slow.recv().await.unwrap();

        assert_eq!(sending.await.unwrap(), Ok(1));
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
    async fn wait_connected_resolves_once_that_session_opens_a_connection() {
        let connections = Connections::default();
        let (_old, _) = connections.register("a", "live", "live.Old");
        assert_eq!(connections.wait_connected("a", "live", SHORT).await, Ok(1));

        let waiting = tokio::spawn({
            let connections = connections.clone();
            async move {
                connections
                    .wait_connected("b", "live", Duration::from_secs(5))
                    .await
            }
        });
        tokio::time::sleep(SHORT).await;
        let (_elsewhere, _) = connections.register("c", "live", "live.Tick");
        let (_other_route, _) = connections.register("b", "other", "live.Tick");
        tokio::time::sleep(SHORT).await;
        assert!(!waiting.is_finished());
        let (_b, _) = connections.register("b", "live", "live.Tick");

        assert_eq!(waiting.await.unwrap(), Ok(1));
    }

    #[tokio::test]
    async fn wait_connected_times_out_or_stops_on_shutdown() {
        let connections = Connections::default();
        assert_eq!(
            connections.wait_connected("a", "live", SHORT).await,
            Err(SendError::Timeout)
        );
        connections.close_all();
        assert_eq!(
            connections.wait_connected("a", "live", SHORT).await,
            Err(SendError::Closed)
        );
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
