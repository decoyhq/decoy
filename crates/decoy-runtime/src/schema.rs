use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::time::parse_duration;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Http,
    Grpc,
    Ws,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawRoute")]
pub struct Route {
    pub id: String,
    pub transport: Transport,
    #[serde(rename = "match")]
    pub route_match: RouteMatch,
    #[serde(default)]
    pub cases: BTreeMap<String, Case>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passthrough: Option<PassthroughTarget>,
    /// Steps that turn request bytes into the envelope's JSON and back (ADR 0002). `None` uses the
    /// transport default: `[grpc-frame, protobuf]` for gRPC, none for WebSocket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline: Option<Vec<PipelineStep>>,
    /// Name of a Pipeline defined in the Decoy config.
    #[serde(rename = "pipeline", skip_serializing_if = "Option::is_none")]
    pub pipeline_name: Option<String>,
}

/// A built-in gRPC Pipeline step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PipelineStep {
    GrpcFrame,
    Protobuf,
}

impl std::fmt::Display for PipelineStep {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GrpcFrame => formatter.write_str("grpc-frame"),
            Self::Protobuf => formatter.write_str("protobuf"),
        }
    }
}

/// `pipeline:` as written: a list of steps, or the name of a `pipelines:` entry in decoy.yaml.
#[derive(Deserialize)]
#[serde(untagged)]
enum PipelineSpec {
    Named(String),
    /// Parsed into steps once the shape is known, so a bad name keeps its own message.
    Steps(Vec<String>),
}

/// A Route as written; `match` is decoded once `transport` says which shape it has.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRoute {
    id: String,
    transport: Transport,
    #[serde(rename = "match")]
    route_match: Value,
    #[serde(default)]
    cases: BTreeMap<String, Case>,
    #[serde(default)]
    passthrough: Option<PassthroughTarget>,
    #[serde(default)]
    pipeline: Option<PipelineSpec>,
}

impl TryFrom<RawRoute> for Route {
    type Error = serde_json::Error;

    fn try_from(raw: RawRoute) -> Result<Self, Self::Error> {
        let route_match = match raw.transport {
            Transport::Http => RouteMatch::Http(serde_json::from_value(raw.route_match)?),
            Transport::Grpc => RouteMatch::Grpc(serde_json::from_value(raw.route_match)?),
            Transport::Ws => RouteMatch::Ws(serde_json::from_value(raw.route_match)?),
        };
        Ok(Self {
            id: raw.id,
            transport: raw.transport,
            route_match,
            cases: raw.cases,
            passthrough: raw.passthrough,
            pipeline_name: match &raw.pipeline {
                Some(PipelineSpec::Named(name)) => Some(name.clone()),
                _ => None,
            },
            pipeline: match raw.pipeline {
                Some(PipelineSpec::Steps(steps)) => Some(
                    steps
                        .into_iter()
                        .map(|name| match name.as_str() {
                            "grpc-frame" => Ok(PipelineStep::GrpcFrame),
                            "protobuf" => Ok(PipelineStep::Protobuf),
                            _ if name.contains('/') => {
                                Err("gRPC Routes take only `grpc-frame` and `protobuf` for now"
                                    .to_owned())
                            }
                            _ => Err(format!("unknown pipeline step `{name}`")),
                        })
                        .collect::<Result<_, _>>()
                        .map_err(<serde_json::Error as serde::de::Error>::custom)?,
                ),
                _ => None,
            },
        })
    }
}

impl Route {
    pub fn from_yaml(input: &str) -> Result<Self, ValidationError> {
        let route: Self = serde_norway::from_str(input)?;
        route.validate()?;
        Ok(route)
    }

    /// Checks that the Pipeline has the form required by the Route transport.
    fn validate_pipeline(&self) -> Result<(), String> {
        match (&self.transport, &self.pipeline_name, &self.pipeline) {
            (Transport::Ws, _, None) | (_, None, None) => Ok(()),
            (Transport::Ws, _, Some(_)) => {
                Err("WebSocket Routes select a named Pipeline".to_owned())
            }
            (Transport::Grpc, None, Some(steps)) => crate::grpc::schema::validate_pipeline(steps),
            (Transport::Grpc, Some(_), _) => {
                Err("gRPC Routes take built-in Pipeline steps".to_owned())
            }
            (Transport::Http, _, _) => Err("HTTP Routes do not take a `pipeline` yet".to_owned()),
        }
    }

    /// Whether the envelope's `message` is protobuf JSON (the default) rather than raw bytes.
    pub fn decodes_protobuf(&self) -> bool {
        self.pipeline
            .as_ref()
            .is_none_or(|steps| steps.contains(&PipelineStep::Protobuf))
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_id("route", &self.id)?;
        self.validate_pipeline()
            .map_err(|reason| ValidationError::InvalidPipeline {
                route: self.id.clone(),
                reason,
            })?;

        if let Some(target) = &self.passthrough {
            target.validate()?;
        }

        let path = match &self.route_match {
            RouteMatch::Http(http) => Some(http.path.as_str()),
            RouteMatch::Ws(ws) => {
                if self.passthrough.is_some() {
                    return Err(ValidationError::InvalidPassthrough {
                        route: self.id.clone(),
                    });
                }
                Some(ws.path.as_str())
            }
            RouteMatch::Grpc(_) => None,
        };
        let path_params = path
            .map(|path| {
                crate::http::path::template_params(path)
                    .map(|params| (path, params))
                    .map_err(|reason| ValidationError::InvalidPath {
                        route: self.id.clone(),
                        reason,
                    })
            })
            .transpose()?;

        if self.cases.is_empty() {
            return Err(ValidationError::EmptyCases {
                route: self.id.clone(),
            });
        }

        let mut on_connect = None;
        for (case_id, case) in &self.cases {
            validate_id("case", case_id)?;

            if case.on.is_some() {
                let reason = if self.transport != Transport::Ws {
                    Some("it is for WebSocket Routes".to_owned())
                } else if !case.case_match.is_null() {
                    Some("an `on: connect` Case takes no `match`".to_owned())
                } else {
                    on_connect.replace(case_id).map(|first| {
                        format!("`{first}` is already this Route's `on: connect` Case")
                    })
                };
                if let Some(reason) = reason {
                    return Err(ValidationError::InvalidTrigger {
                        route: self.id.clone(),
                        case: case_id.clone(),
                        reason,
                    });
                }
            }

            if case.behaviors.is_empty() {
                return Err(ValidationError::EmptyBehaviors {
                    route: self.id.clone(),
                    case: case_id.clone(),
                });
            }

            for (behavior_id, behavior) in &case.behaviors {
                validate_id("behavior", behavior_id)?;
                let checked = match (&self.transport, behavior) {
                    (Transport::Http, Behavior::Response(response)) => {
                        crate::template::validate_response(response)
                    }
                    (Transport::Grpc, Behavior::Response(response)) => {
                        crate::grpc::schema::validate_response(response, self.decodes_protobuf())
                    }
                    (Transport::Ws, Behavior::Response(response)) => {
                        crate::ws::schema::validate_response(response)
                    }
                    (Transport::Http, Behavior::Passthrough(passthrough)) => passthrough
                        .target
                        .as_ref()
                        .map_or(Ok(()), PassthroughTarget::validate)
                        .map_err(|error| error.to_string()),
                    (Transport::Grpc, Behavior::Passthrough(_)) => {
                        Err("gRPC Routes do not support passthrough".to_owned())
                    }
                    (Transport::Ws, Behavior::Passthrough(_)) => {
                        Err("WebSocket Routes do not support passthrough".to_owned())
                    }
                };
                checked.map_err(|reason| ValidationError::InvalidBehavior {
                    address: format!("{}:{case_id}:{behavior_id}", self.id),
                    reason,
                })?;
            }

            crate::matching::validate_case_match(&self.transport, &case.case_match)
                .and_then(|()| {
                    let (Some((path, params)), Some(Value::Object(patterns))) =
                        (&path_params, case.case_match.get("pathParams"))
                    else {
                        return Ok(());
                    };
                    match patterns.keys().find(|key| !params.contains(&key.as_str())) {
                        Some(key) => Err(format!(
                            "`pathParams.{key}` is not a parameter of path `{path}`"
                        )),
                        None => Ok(()),
                    }
                })
                .map_err(|reason| ValidationError::InvalidMatch {
                    route: self.id.clone(),
                    case: case_id.clone(),
                    reason,
                })?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum RouteMatch {
    Http(HttpRouteMatch),
    Grpc(GrpcRouteMatch),
    Ws(WsRouteMatch),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpRouteMatch {
    pub method: HttpMethod,
    pub path: String,
}

/// A gRPC method address: the fully qualified service and the method name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrpcRouteMatch {
    pub service: String,
    pub method: String,
}

/// A WebSocket upgrade address: the path (with `{param}`s, as for HTTP) and query params the
/// upgrade must carry; other params are ignored. More `query` keys win when several Routes match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WsRouteMatch {
    pub path: String,
    /// Value `*` accepts any value.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub query: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    #[default]
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
}

impl HttpMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    /// WebSocket: `connect` runs the Case once when the socket opens, instead of per message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<CaseTrigger>,
    #[serde(default, rename = "match")]
    pub case_match: Value,
    #[serde(default)]
    pub behaviors: BTreeMap<String, Behavior>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaseTrigger {
    Connect,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawBehavior", into = "RawBehavior")]
pub enum Behavior {
    Response(ResponseBehavior),
    Passthrough(PassthroughBehavior),
}

impl Behavior {
    /// How long to wait before responding or forwarding.
    pub fn delay(&self) -> Option<Duration> {
        match self {
            Self::Response(response) => response.delay,
            Self::Passthrough(passthrough) => passthrough.delay,
        }
    }

    pub fn kind(&self) -> BehaviorKind {
        match self {
            Self::Response(_) => BehaviorKind::Response,
            Self::Passthrough(_) => BehaviorKind::Passthrough,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BehaviorKind {
    Response,
    Passthrough,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseBehavior {
    /// An integer, or a `${ }` template that renders to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Value>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
    /// gRPC: the response message, or the status message when `status` is an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Value>,
    /// gRPC: response metadata (sent as trailers).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    /// Server-streaming gRPC: response messages sent in order. The call then ends with `status`,
    /// or without one stays open for Control API `send`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<Vec<StreamItem>>,
    /// WebSocket: messages sent in order, each `delay` after the one before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<MessageItem>>,
    /// WebSocket: close the connection after the messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close: Option<WsClose>,
    #[serde(skip)]
    pub delay: Option<Duration>,
}

/// A WebSocket close frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WsClose {
    /// 1000, or an application code 3000–4999.
    #[serde(default = "WsClose::normal")]
    pub code: u16,
    #[serde(default)]
    pub reason: String,
}

impl WsClose {
    fn normal() -> u16 {
        1000
    }
}

/// One message of a server-streaming answer, sent `delay` after the one before it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "MessageItem", into = "MessageItem")]
pub struct StreamItem {
    pub message: Value,
    pub delay: Option<Duration>,
}

impl From<MessageItem> for StreamItem {
    fn from(item: MessageItem) -> Self {
        Self {
            message: item.payload,
            delay: item.delay,
        }
    }
}

impl From<StreamItem> for MessageItem {
    fn from(item: StreamItem) -> Self {
        Self {
            payload: item.message,
            delay: item.delay,
        }
    }
}

/// Payload and delay for one message in a WebSocket Behavior.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawMessageItem", into = "RawMessageItem")]
pub struct MessageItem {
    pub payload: Value,
    pub delay: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMessageItem {
    payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delay: Option<Value>,
}

impl TryFrom<RawMessageItem> for MessageItem {
    type Error = ValidationError;

    fn try_from(raw: RawMessageItem) -> Result<Self, Self::Error> {
        Ok(Self {
            payload: raw.payload,
            delay: raw.delay.as_ref().map(parse_delay).transpose()?,
        })
    }
}

impl From<MessageItem> for RawMessageItem {
    fn from(item: MessageItem) -> Self {
        Self {
            payload: item.payload,
            delay: item.delay.map(format_delay),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PassthroughBehavior {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<PassthroughTarget>,
    #[serde(skip)]
    pub delay: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PassthroughTarget {
    #[serde(rename = "baseUrl")]
    pub base_url: String,
}

impl PassthroughTarget {
    fn validate(&self) -> Result<(), ValidationError> {
        if url::Url::parse(&self.base_url)
            .is_ok_and(|parsed| matches!(parsed.scheme(), "http" | "https"))
        {
            Ok(())
        } else {
            Err(ValidationError::InvalidTarget(self.base_url.clone()))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBehavior {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<BehaviorKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status: Option<Value>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    body: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    metadata: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stream: Option<Vec<StreamItem>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    messages: Option<Vec<MessageItem>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    close: Option<WsClose>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<PassthroughTarget>,
    /// Integer milliseconds or a duration string with `ms` and `s` units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delay: Option<Value>,
}

impl TryFrom<RawBehavior> for Behavior {
    type Error = ValidationError;

    fn try_from(raw: RawBehavior) -> Result<Self, Self::Error> {
        let delay = raw.delay.as_ref().map(parse_delay).transpose()?;
        let kind = raw.kind.unwrap_or(BehaviorKind::Response);
        let misplaced = match kind {
            BehaviorKind::Response => raw.target.is_some().then_some("target"),
            BehaviorKind::Passthrough => [
                ("status", raw.status.is_some()),
                ("headers", !raw.headers.is_empty()),
                ("body", raw.body.is_some()),
                ("message", raw.message.is_some()),
                ("metadata", !raw.metadata.is_empty()),
                ("stream", raw.stream.is_some()),
                ("messages", raw.messages.is_some()),
                ("close", raw.close.is_some()),
            ]
            .into_iter()
            .find_map(|(field, set)| set.then_some(field)),
        };
        if let Some(field) = misplaced {
            let kind = match kind {
                BehaviorKind::Response => "response",
                BehaviorKind::Passthrough => "passthrough",
            };
            return Err(ValidationError::MisplacedBehaviorField { kind, field });
        }
        match kind {
            BehaviorKind::Response => Ok(Self::Response(ResponseBehavior {
                status: raw.status,
                headers: raw.headers,
                body: raw.body,
                message: raw.message,
                metadata: raw.metadata,
                stream: raw.stream,
                messages: raw.messages,
                close: raw.close,
                delay,
            })),
            BehaviorKind::Passthrough => Ok(Self::Passthrough(PassthroughBehavior {
                target: raw.target,
                delay,
            })),
        }
    }
}

fn parse_delay(delay: &Value) -> Result<Duration, ValidationError> {
    let parsed = match delay {
        Value::Number(millis) => millis.as_u64().map(Duration::from_millis),
        Value::String(text) => parse_duration(text),
        _ => None,
    };
    parsed.ok_or_else(|| ValidationError::InvalidDelay(delay.to_string()))
}

fn format_delay(delay: Duration) -> Value {
    Value::from(u64::try_from(delay.as_millis()).unwrap_or(u64::MAX))
}

impl From<Behavior> for RawBehavior {
    fn from(value: Behavior) -> Self {
        let delay = value.delay().map(format_delay);
        match value {
            Behavior::Response(response) => Self {
                kind: Some(BehaviorKind::Response),
                status: response.status,
                headers: response.headers,
                body: response.body,
                message: response.message,
                metadata: response.metadata,
                stream: response.stream,
                messages: response.messages,
                close: response.close,
                target: None,
                delay,
            },
            Behavior::Passthrough(passthrough) => Self {
                kind: Some(BehaviorKind::Passthrough),
                status: None,
                headers: BTreeMap::new(),
                body: None,
                message: None,
                metadata: BTreeMap::new(),
                stream: None,
                messages: None,
                close: None,
                target: passthrough.target,
                delay,
            },
        }
    }
}

#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("failed to parse route yaml: {0}")]
    Yaml(#[from] serde_norway::Error),
    #[error("{kind} id `{id}` cannot be empty")]
    EmptyId { kind: &'static str, id: String },
    #[error(
        "{kind} id `{id}` cannot contain `:` because collection addresses use route:case:behavior"
    )]
    IdContainsColon { kind: &'static str, id: String },
    #[error("route `{route}` must define at least one case")]
    EmptyCases { route: String },
    #[error("case `{route}:{case}` must define at least one behavior")]
    EmptyBehaviors { route: String, case: String },
    #[error("delay {0} must be integer milliseconds or a duration such as `500ms` or `2s`")]
    InvalidDelay(String),
    #[error("route `{route}` has an invalid path: {reason}")]
    InvalidPath { route: String, reason: String },
    #[error("route `{route}` has an invalid pipeline: {reason}")]
    InvalidPipeline { route: String, reason: String },
    #[error("{kind} behavior does not take `{field}`")]
    MisplacedBehaviorField {
        kind: &'static str,
        field: &'static str,
    },
    #[error("route `{route}`: WebSocket Routes do not support passthrough")]
    InvalidPassthrough { route: String },
    #[error("passthrough target `{0}` must be an http or https URL")]
    InvalidTarget(String),
    #[error("behavior `{address}` is invalid: {reason}")]
    InvalidBehavior { address: String, reason: String },
    #[error("case `{route}:{case}` has an invalid `on`: {reason}")]
    InvalidTrigger {
        route: String,
        case: String,
        reason: String,
    },
    #[error("case `{route}:{case}` has an invalid match: {reason}")]
    InvalidMatch {
        route: String,
        case: String,
        reason: String,
    },
}

pub(crate) fn validate_id(kind: &'static str, id: &str) -> Result<(), ValidationError> {
    if id.trim().is_empty() {
        return Err(ValidationError::EmptyId {
            kind,
            id: id.to_owned(),
        });
    }

    if id.contains(':') {
        return Err(ValidationError::IdContainsColon {
            kind,
            id: id.to_owned(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn http_methods_print_as_written_in_routes() {
        for name in ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"] {
            let method: HttpMethod = serde_json::from_value(serde_json::json!(name)).unwrap();
            assert_eq!(method.as_str(), name);
        }
    }

    #[test]
    fn parses_http_route_case_behavior_schema() {
        let route = Route::from_yaml(
            r#"
id: get-user
transport: http
match:
  method: GET
  path: /users/{id}

cases:
  user-123:
    match:
      pathParams:
        id: "123"
    behaviors:
      success:
        status: 200
        body:
          id: "123"
"#,
        )
        .unwrap();

        assert_eq!(route.id, "get-user");
        assert_eq!(route.transport, Transport::Http);
        assert_eq!(
            route.route_match,
            RouteMatch::Http(HttpRouteMatch {
                method: HttpMethod::Get,
                path: "/users/{id}".to_owned(),
            })
        );
        assert_eq!(
            route.cases["user-123"].behaviors["success"].kind(),
            BehaviorKind::Response
        );
    }

    #[test]
    fn infers_response_behavior_kind() {
        let route = Route::from_yaml(
            r#"
id: get-user
transport: http
match:
  method: GET
  path: /users/{id}
cases:
  user-123:
    behaviors:
      success:
        status: 200
        body: ok
"#,
        )
        .unwrap();

        assert!(matches!(
            route.cases["user-123"].behaviors["success"],
            Behavior::Response(_)
        ));
    }

    #[test]
    fn parses_explicit_passthrough_behavior() {
        let route = Route::from_yaml(
            r#"
id: get-user
transport: http
match:
  method: GET
  path: /users/{id}
cases:
  any:
    behaviors:
      real:
        kind: passthrough
        target:
          baseUrl: https://api.example.test
"#,
        )
        .unwrap();

        assert_eq!(
            route.cases["any"].behaviors["real"].kind(),
            BehaviorKind::Passthrough
        );
    }

    #[test]
    fn parses_grpc_route_match_and_rejects_mismatched_shapes() {
        let route = Route::from_yaml(
            r#"
id: approvals-get
transport: grpc
match:
  service: approvals.v1.Approvals
  method: GetApproval
cases:
  any:
    behaviors:
      ok:
        message: { id: a-1 }
"#,
        )
        .unwrap();
        assert_eq!(
            route.route_match,
            RouteMatch::Grpc(GrpcRouteMatch {
                service: "approvals.v1.Approvals".to_owned(),
                method: "GetApproval".to_owned(),
            })
        );

        let error = Route::from_yaml(
            r#"
id: approvals-get
transport: grpc
match: { method: GET, path: /x }
cases:
  any:
    behaviors:
      ok: {}
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("unknown field `path`"), "{error}");
    }

    #[test]
    fn http_paths_take_params_only_as_whole_segments() {
        let route = |path: &str| {
            Route::from_yaml(&format!(
                "id: files\ntransport: http\nmatch: {{ method: GET, path: '{path}' }}\ncases:\n  any:\n    behaviors:\n      ok: {{ status: 200 }}\n"
            ))
        };
        assert!(route("/files/{name}").is_ok());
        let error = route("/files/{name}.json").unwrap_err().to_string();
        assert_eq!(
            error,
            "route `files` has an invalid path: segment `{name}.json` must be literal text or a whole `{param}`"
        );
        assert!(route("/files/{}").is_err());
        assert!(route("/files/{a}{b}").is_err());
    }

    #[test]
    fn path_params_must_name_params_of_the_path() {
        let route = |params: &str| {
            Route::from_yaml(&format!(
                "id: users\ntransport: http\nmatch: {{ method: GET, path: '/users/{{id}}' }}\ncases:\n  one:\n    match: {{ pathParams: {params} }}\n    behaviors:\n      ok: {{ status: 200 }}\n"
            ))
        };
        assert!(route("{ id: '1' }").is_ok());
        assert!(route("'${ pathParams.id == `1` }'").is_ok());
        let error = route("{ userId: '1' }").unwrap_err().to_string();
        assert_eq!(
            error,
            "case `users:one` has an invalid match: `pathParams.userId` is not a parameter of path `/users/{id}`"
        );
    }

    #[test]
    fn http_behaviors_reject_grpc_fields() {
        let error = Route::from_yaml(
            r#"
id: users
transport: http
match: { method: GET, path: /users }
cases:
  any:
    behaviors:
      ok:
        message: { id: 1 }
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("`message` and `metadata` are for gRPC Routes"),
            "{error}"
        );
    }

    /// Pins today's split: gRPC streams use `stream`. When `stream` becomes `messages` (Q11),
    /// this test must change with it.
    #[test]
    fn grpc_and_http_behaviors_reject_websocket_fields() {
        let error = |transport: &str, address: &str, behavior: &str| {
            Route::from_yaml(&format!(
                "id: r\ntransport: {transport}\nmatch: {address}\ncases:\n  any:\n    behaviors:\n      b: {behavior}\n"
            ))
            .unwrap_err()
            .to_string()
        };
        let grpc = |behavior| {
            error(
                "grpc",
                "{ service: approvals.v1.Approvals, method: WatchApprovals }",
                behavior,
            )
        };
        let http = |behavior| error("http", "{ method: GET, path: /r }", behavior);
        for behavior in [
            "{ messages: [{ payload: { id: a-1 } }] }",
            "{ close: { code: 4001 } }",
        ] {
            let error = grpc(behavior);
            assert!(
                error.contains(
                    "`messages` and `close` are for WebSocket Routes; gRPC streams use `stream`"
                ),
                "{error}"
            );
            let error = http(behavior);
            assert!(
                error.contains("`messages` and `close` are for WebSocket Routes"),
                "{error}"
            );
        }
    }

    #[test]
    fn parses_a_stream_list_with_per_message_delays() {
        let route = Route::from_yaml(
            r#"
id: watch
transport: grpc
match: { service: approvals.v1.Approvals, method: WatchApprovals }
cases:
  any:
    behaviors:
      progress:
        stream:
          - { payload: { id: a-1 } }
          - { payload: { id: a-2 }, delay: 500ms }
        status: ABORTED
"#,
        )
        .unwrap();
        let Behavior::Response(response) = &route.cases["any"].behaviors["progress"] else {
            panic!("expected a response behavior")
        };
        assert_eq!(
            response.stream,
            Some(vec![
                StreamItem {
                    message: serde_json::json!({ "id": "a-1" }),
                    delay: None,
                },
                StreamItem {
                    message: serde_json::json!({ "id": "a-2" }),
                    delay: Some(Duration::from_millis(500)),
                },
            ])
        );

        assert!(serde_norway::from_str::<StreamItem>("{ message: {} }").is_err());

        let error = route_error("", "        stream: [{ payload: { id: 1 } }]");
        assert!(
            error.contains("`stream` is for server-streaming gRPC Routes"),
            "{error}"
        );
        let error = route_error("", "        kind: passthrough\n        stream: []");
        assert!(
            error.contains("passthrough behavior does not take `stream`"),
            "{error}"
        );
    }

    #[test]
    fn rejects_colon_in_address_parts() {
        let error = Route::from_yaml(
            r#"
id: get:user
transport: http
match:
  method: GET
  path: /users/{id}
cases:
  any:
    behaviors:
      success:
        status: 200
"#,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ValidationError::IdContainsColon { kind: "route", .. }
        ));
    }

    #[test]
    fn parses_behavior_delay_as_millis_or_duration() {
        let route = |delay: &str| {
            Route::from_yaml(&format!(
                r#"
id: slow
transport: http
match:
  method: GET
  path: /slow
cases:
  any:
    behaviors:
      wait:
        delay: {delay}
      forward:
        kind: passthrough
        delay: {delay}
"#
            ))
        };
        for (input, millis) in [
            ("0", 0),
            ("250", 250),
            ("'500ms'", 500),
            ("2s", 2000),
            ("1.5s", 1_500),
            ("500ms 1s 500ms", 2_000),
        ] {
            let route = route(input).unwrap();
            for behavior in route.cases["any"].behaviors.values() {
                assert_eq!(
                    behavior.delay(),
                    Some(Duration::from_millis(millis)),
                    "{input}"
                );
            }
        }
        let error = route("fast").unwrap_err().to_string();
        assert!(error.contains("must be integer milliseconds"), "{error}");
        assert!(route("-5").is_err());
        assert!(route("1m 30s").is_err());
    }

    fn route_error(extra_route: &str, behavior: &str) -> String {
        Route::from_yaml(&format!(
            r#"
id: users
transport: http
match: {{ method: GET, path: /users }}
{extra_route}
cases:
  any:
    behaviors:
      ok:
{behavior}
"#
        ))
        .unwrap_err()
        .to_string()
    }

    #[test]
    fn rejects_unknown_route_key() {
        let error = route_error("extends: base", "        status: 200");
        assert!(error.contains("unknown field `extends`"), "{error}");
    }

    #[test]
    fn rejects_unknown_case_key_instead_of_making_a_catch_all() {
        let error = Route::from_yaml(
            r#"
id: users
transport: http
match: { method: GET, path: /users }
cases:
  admin:
    matches: { query: { role: admin } }
    behaviors:
      ok: { status: 200 }
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("unknown field `matches`"), "{error}");
    }

    #[test]
    fn rejects_unknown_behavior_key() {
        let error = route_error("", "        status: 200\n        delay_ms: 500");
        assert!(error.contains("unknown field `delay_ms`"), "{error}");
    }

    #[test]
    fn rejects_unknown_passthrough_target_key() {
        let error = route_error(
            "passthrough: { baseUrl: 'http://up', timeout: 5 }",
            "        status: 200",
        );
        assert!(error.contains("unknown field `timeout`"), "{error}");
    }

    #[test]
    fn rejects_response_fields_on_a_passthrough_behavior() {
        let error = route_error("", "        kind: passthrough\n        status: 200");
        assert!(
            error.contains("passthrough behavior does not take `status`"),
            "{error}"
        );
    }

    #[test]
    fn rejects_a_target_on_a_response_behavior() {
        let error = route_error(
            "",
            "        status: 200\n        target: { baseUrl: 'http://up' }",
        );
        assert!(
            error.contains("response behavior does not take `target`"),
            "{error}"
        );
    }

    #[test]
    fn passthrough_targets_must_be_http_urls() {
        let error = route_error(
            "passthrough: { baseUrl: 'localhost:3000' }",
            "        status: 200",
        );
        assert!(error.contains("`localhost:3000`"), "{error}");
        let error = route_error(
            "",
            "        kind: passthrough\n        target: { baseUrl: 'ftp://up' }",
        );
        assert!(error.contains("`ftp://up`"), "{error}");
    }
}
