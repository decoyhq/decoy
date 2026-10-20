use super::{GrpcReply, GrpcRequest};
use crate::connections::Connections;
use crate::engine::SessionView;
use crate::events::{CapturedBody, CapturedResponse};
use crate::schema::Transport;

impl SessionView {
    /// Executes the call and, if anyone subscribed, emits its [`RequestEvent`](crate::events::RequestEvent).
    /// A server-streaming call emits it once, as it opens; an open stream registers in
    /// `connections` for Control API `send`.
    pub async fn execute_grpc(&self, request: GrpcRequest, connections: &Connections) -> GrpcReply {
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
        match call.stream {
            Some(stream) => GrpcReply::Stream(stream.serve(connections, &self.session)),
            None => GrpcReply::Unary(call.response),
        }
    }
}
