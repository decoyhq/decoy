//! Converts WebSocket frames to Case inputs and Behavior payloads to frames.

use super::Inbound;

use rmpv::Value as Msgpack;
use serde_json::Value;

pub use crate::pipeline::{Pipeline as WsPipeline, PipelineError, check_routes};
use crate::plugins::value::{to_json, to_msgpack};
use crate::plugins::{CallConnection, Frame, Op};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Encoded {
    Text(String),
    Binary(Vec<u8>),
}

/// Turns frames into messages and back: as text, or through the Route's Pipeline.
pub(crate) struct Codec {
    pub(crate) pipeline: Option<WsPipeline>,
    pub(crate) call: CallConnection,
}

impl Codec {
    /// The inbound message a frame stands for; `None` after a plugin error (logged).
    pub(crate) async fn decode<'a>(&self, frame: Frame, payload: &'a [u8]) -> Option<Inbound<'a>> {
        let Some(pipeline) = self
            .pipeline
            .as_ref()
            .filter(|pipeline| pipeline.has_input())
        else {
            return Some(match frame {
                Frame::Binary => Inbound::Binary(payload),
                Frame::Text => {
                    Inbound::Text(std::str::from_utf8(payload).expect("text frames are UTF-8"))
                }
            });
        };
        let decoded = pipeline
            .run_ws(
                Op::Input,
                Msgpack::Binary(payload.to_vec()),
                Some(frame),
                &self.call,
            )
            .await
            .and_then(|value| {
                match value {
                    Msgpack::Binary(bytes) if pipeline.input_ends_in_command() => {
                        serde_json::from_slice(&bytes).map_err(|error| error.to_string())
                    }
                    value => to_json(&value),
                }
                .map_err(|reason| pipeline.invalid_result(Op::Input, reason))
            });
        match decoded {
            Ok(message) => Some(Inbound::Decoded(message)),
            Err(error @ PipelineError::Miss { .. }) => Some(Inbound::Missed {
                payload,
                reason: error.to_string(),
            }),
            Err(error) => {
                crate::events::error(self.report(
                    &error,
                    "decoding a frame",
                    &format!("\n  frame: {}", preview(frame, payload)),
                ));
                None
            }
        }
    }

    /// The frame for `message`; `None` after a plugin error (logged).
    pub(crate) async fn encode(&self, message: &Value, origin: &str) -> Option<Encoded> {
        let Some(pipeline) = self
            .pipeline
            .as_ref()
            .filter(|pipeline| pipeline.has_output())
        else {
            return Some(Encoded::Text(crate::ws::encode_text(message)));
        };
        let encoded = pipeline
            .run_ws(Op::Output, to_msgpack(message), None, &self.call)
            .await
            .and_then(|value| match value {
                Msgpack::String(text) => text
                    .into_str()
                    .map(Encoded::Text)
                    .ok_or_else(|| pipeline.invalid_result(Op::Output, "returned non-UTF-8 text")),
                Msgpack::Binary(bytes) => Ok(Encoded::Binary(bytes)),
                _ => Err(pipeline.invalid_result(Op::Output, "expected text or bytes")),
            });
        match encoded {
            Ok(encoded) => Some(encoded),
            Err(error) => {
                crate::events::error(self.report(&error, origin, ""));
                None
            }
        }
    }

    /// One error event with what's needed to fix it: where, what failed, the value, the stack.
    fn report(&self, error: &PipelineError, doing: &str, value: &str) -> String {
        let CallConnection { id, route, session } = &self.call;
        let stack = error.stack().map_or(String::new(), |stack| {
            let lines: Vec<_> = stack.lines().map(|line| format!("\n    {line}")).collect();
            format!("\n  stack:{}", lines.concat())
        });
        format!("ws `{route}` connection {id} (session {session}), {doing}: {error}{value}{stack}")
    }

    /// Lets the plugins drop what they kept for this connection.
    pub(crate) fn closed(&self) {
        for plugin in self.pipeline.iter().flat_map(WsPipeline::plugins) {
            plugin.close(&self.call.id);
        }
    }
}

/// `binary, 12 bytes: 0a 03 41 …` (the first 32), for error events.
pub fn preview(frame: Frame, payload: &[u8]) -> String {
    const SHOWN: usize = 32;
    let kind = match frame {
        Frame::Text => "text",
        Frame::Binary => "binary",
    };
    let hex: Vec<_> = payload
        .iter()
        .take(SHOWN)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let more = if payload.len() > SHOWN { " …" } else { "" };
    format!("{kind}, {} bytes: {}{more}", payload.len(), hex.join(" "))
}
