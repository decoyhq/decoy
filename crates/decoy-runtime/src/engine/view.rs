use std::collections::BTreeMap;
use std::sync::Arc;

use super::{Catalog, Selection};
use crate::events::{CapturedBody, CapturedResponse, Event, Outcome, RequestEvent};
use crate::schema::Transport;

/// What one request needs: the shared Catalog and a copy of its Session's Selection, taken under
/// the read lock so the lock isn't held across `delay`s or passthrough.
#[derive(Debug, Clone)]
pub struct SessionView {
    pub(crate) catalog: Arc<Catalog>,
    pub(crate) session: String,
    pub(crate) selection: Selection,
    pub(crate) events: crate::events::Sender,
}

/// The request half of a [`RequestEvent`], captured before the request is consumed.
pub(crate) struct Started {
    at: std::time::SystemTime,
    clock: std::time::Instant,
    method: String,
    url: String,
    headers: BTreeMap<String, String>,
}

// The execute drivers (`execute_http`, `execute_grpc`) live in `http/view.rs` and `grpc/view.rs`,
// so the core imports no transport I/O.
impl SessionView {
    pub(crate) fn start(
        &self,
        method: &str,
        url: &str,
        headers: &BTreeMap<String, String>,
    ) -> Started {
        Started {
            at: std::time::SystemTime::now(),
            clock: std::time::Instant::now(),
            method: method.to_owned(),
            url: url.to_owned(),
            headers: headers.clone(),
        }
    }

    pub(crate) fn emit(
        &self,
        transport: Transport,
        started: Started,
        body: CapturedBody,
        outcome: Outcome,
        response: CapturedResponse,
    ) {
        // An error only means every subscriber left meanwhile.
        let _ = self.events.send(Event::Request(Arc::new(RequestEvent {
            id: crate::events::next_id(),
            at: started.at,
            transport,
            session: self.session.clone(),
            method: started.method,
            url: started.url,
            headers: started.headers,
            body,
            outcome,
            response,
            duration: started.clock.elapsed(),
        })));
    }
}
