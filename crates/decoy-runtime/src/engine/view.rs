use std::collections::BTreeMap;
use std::sync::Arc;

use futures_util::StreamExt as _;

use super::{Catalog, HttpExecutionOutcome, HttpExecutionRequest, Selection};
use crate::events::{CapturedBody, CapturedResponse, Event, Outcome, RequestEvent};
use crate::grpc::{GrpcRequest, GrpcResponse};
use crate::http_forward::UpstreamBody;
use crate::schema::Transport;

/// What one request needs: the shared Catalog and a copy of its Session's Selection, taken under
/// the read lock so the lock isn't held across `delay`s or passthrough.
#[derive(Debug, Clone)]
pub struct SessionView {
    pub(super) catalog: Arc<Catalog>,
    pub(super) session: String,
    pub(super) selection: Selection,
    pub(super) events: crate::events::Sender,
}

/// The request half of a [`RequestEvent`], captured before the request is consumed.
struct Started {
    at: std::time::SystemTime,
    clock: std::time::Instant,
    method: String,
    url: String,
    headers: BTreeMap<String, String>,
}

impl SessionView {
    /// Executes the request and, if anyone subscribed, emits its [`RequestEvent`].
    pub async fn execute_http(&self, request: HttpExecutionRequest) -> HttpExecutionOutcome {
        // No subscriber (headless `serve`): skip every capture and clone.
        let started = (self.events.receiver_count() > 0).then(|| {
            (
                self.start(
                    request.method.as_str(),
                    &request.path_and_query,
                    &request.headers,
                ),
                CapturedBody::raw(&request.body),
            )
        });
        let (outcome, explained) = self
            .catalog
            .execute_http_with_overrides(
                &self.selection.collection,
                &self.selection.route_overrides,
                request,
                started.is_some(),
            )
            .await;
        let (Some((started, body)), Some(explained)) = (started, explained) else {
            return outcome;
        };
        let mut outcome = outcome;
        let response = match &mut outcome {
            HttpExecutionOutcome::Response { response, .. } => response,
            HttpExecutionOutcome::ForwardingError { source, .. } => {
                let response = source.response();
                let captured = CapturedResponse {
                    status: response.status,
                    headers: response.headers,
                    body: CapturedBody::raw(&response.body),
                };
                self.emit(Transport::Http, started, body, explained, captured);
                return outcome;
            }
        };
        let mut captured = CapturedResponse {
            status: response.status,
            headers: response.headers.clone(),
            body: CapturedBody::raw(&response.body),
        };
        let Some(stream) = response.stream.take() else {
            self.emit(Transport::Http, started, body, explained, captured);
            return outcome;
        };
        // A streamed body is captured as it passes, and the event goes out once it ends or the
        // client hangs up.
        let view = self.clone();
        let mut capture = crate::events::StreamCapture::new(move |streamed| {
            captured.body = streamed;
            view.emit(Transport::Http, started, body, explained, captured);
        });
        response.stream = Some(UpstreamBody(
            stream
                .0
                .inspect(move |chunk| {
                    if let Ok(chunk) = chunk {
                        capture.push(chunk);
                    }
                })
                .boxed(),
        ));
        outcome
    }

    /// Executes the call and, if anyone subscribed, emits its [`RequestEvent`].
    pub async fn execute_grpc(&self, request: GrpcRequest) -> GrpcResponse {
        let capture = self.events.receiver_count() > 0;
        let started = capture.then(|| {
            self.start(
                "POST",
                &format!("/{}/{}", request.service, request.method),
                &request.metadata,
            )
        });
        let raw_body = capture.then(|| CapturedBody::raw(&request.body));
        let call = self
            .catalog
            .execute_grpc(
                &self.selection.collection,
                &self.selection.route_overrides,
                request,
                capture,
            )
            .await;
        if let (Some(started), Some(raw_body), Some(outcome)) = (started, raw_body, call.outcome) {
            let response = &call.response;
            let mut headers: Vec<_> = response.metadata.clone().into_iter().collect();
            if !response.status_message.is_empty() {
                headers.push(("grpc-message".to_owned(), response.status_message.clone()));
            }
            self.emit(
                Transport::Grpc,
                started,
                call.request
                    .as_ref()
                    .map_or(raw_body, CapturedBody::decoded),
                outcome,
                CapturedResponse {
                    status: response.status.into(),
                    headers,
                    body: call
                        .message
                        .as_ref()
                        .map_or_else(|| CapturedBody::raw(&response.body), CapturedBody::decoded),
                },
            );
        }
        call.response
    }

    fn start(&self, method: &str, url: &str, headers: &BTreeMap<String, String>) -> Started {
        Started {
            at: std::time::SystemTime::now(),
            clock: std::time::Instant::now(),
            method: method.to_owned(),
            url: url.to_owned(),
            headers: headers.clone(),
        }
    }

    fn emit(
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
