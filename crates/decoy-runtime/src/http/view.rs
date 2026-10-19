use futures_util::StreamExt as _;

use super::forward::UpstreamBody;
use super::{HttpExecutionOutcome, HttpExecutionRequest};
use crate::engine::SessionView;
use crate::events::{CapturedBody, CapturedResponse};
use crate::schema::Transport;

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
}
