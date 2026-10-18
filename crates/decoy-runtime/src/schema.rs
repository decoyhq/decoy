use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Http,
    Grpc,
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
    /// transport default: `[grpc-frame, protobuf]` for gRPC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline: Option<Vec<PipelineStep>>,
}

/// A built-in Pipeline step. Plugin steps come later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PipelineStep {
    /// gRPC length-prefixed message framing.
    GrpcFrame,
    /// Protobuf bytes ⇄ protobuf JSON, typed by the gRPC method.
    Protobuf,
}

/// A Route as written; `match` is decoded once `transport` says which shape it has.
#[derive(Deserialize)]
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
    pipeline: Option<Vec<PipelineStep>>,
}

impl TryFrom<RawRoute> for Route {
    type Error = serde_json::Error;

    fn try_from(raw: RawRoute) -> Result<Self, Self::Error> {
        let route_match = match raw.transport {
            Transport::Http => RouteMatch::Http(serde_json::from_value(raw.route_match)?),
            Transport::Grpc => RouteMatch::Grpc(serde_json::from_value(raw.route_match)?),
        };
        Ok(Self {
            id: raw.id,
            transport: raw.transport,
            route_match,
            cases: raw.cases,
            passthrough: raw.passthrough,
            pipeline: raw.pipeline,
        })
    }
}

impl Route {
    pub fn from_yaml(input: &str) -> Result<Self, ValidationError> {
        let route: Self = serde_yaml::from_str(input)?;
        route.validate()?;
        Ok(route)
    }

    /// Whether the envelope's `message` is protobuf JSON (the default) rather than raw bytes.
    pub fn decodes_protobuf(&self) -> bool {
        self.pipeline
            .as_ref()
            .is_none_or(|steps| steps.contains(&PipelineStep::Protobuf))
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_id("route", &self.id)?;

        if self.transport == Transport::Http && self.pipeline.is_some() {
            return Err(ValidationError::InvalidPipeline {
                route: self.id.clone(),
                reason: "HTTP Routes do not take a `pipeline` yet".to_owned(),
            });
        }

        if let Some(steps) = &self.pipeline {
            crate::grpc_schema::validate_pipeline(steps).map_err(|reason| {
                ValidationError::InvalidPipeline {
                    route: self.id.clone(),
                    reason,
                }
            })?;
        }

        if self.cases.is_empty() {
            return Err(ValidationError::EmptyCases {
                route: self.id.clone(),
            });
        }

        for (case_id, case) in &self.cases {
            validate_id("case", case_id)?;

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
                        crate::grpc_schema::validate_response(response, self.decodes_protobuf())
                    }
                    (Transport::Http, Behavior::Passthrough(_)) => Ok(()),
                    (Transport::Grpc, Behavior::Passthrough(_)) => {
                        Err("gRPC Routes do not support passthrough".to_owned())
                    }
                };
                checked.map_err(|reason| ValidationError::InvalidBehavior {
                    address: format!("{}:{case_id}:{behavior_id}", self.id),
                    reason,
                })?;
            }

            crate::matching::validate_case_match(&self.transport, &case.case_match).map_err(
                |reason| ValidationError::InvalidMatch {
                    route: self.id.clone(),
                    case: case_id.clone(),
                    reason,
                },
            )?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum RouteMatch {
    Http(HttpRouteMatch),
    Grpc(GrpcRouteMatch),
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Case {
    #[serde(default, rename = "match")]
    pub case_match: Value,
    #[serde(default)]
    pub behaviors: BTreeMap<String, Behavior>,
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
    #[serde(skip)]
    pub delay: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PassthroughBehavior {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<PassthroughTarget>,
    #[serde(skip)]
    pub delay: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassthroughTarget {
    #[serde(rename = "baseUrl")]
    pub base_url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    target: Option<PassthroughTarget>,
    /// Integer milliseconds or a duration string such as `500ms` or `2s`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delay: Option<Value>,
}

impl TryFrom<RawBehavior> for Behavior {
    type Error = ValidationError;

    fn try_from(raw: RawBehavior) -> Result<Self, Self::Error> {
        let delay = raw.delay.as_ref().map(parse_delay).transpose()?;
        match raw.kind.unwrap_or(BehaviorKind::Response) {
            BehaviorKind::Response => Ok(Self::Response(ResponseBehavior {
                status: raw.status,
                headers: raw.headers,
                body: raw.body,
                message: raw.message,
                metadata: raw.metadata,
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
        Value::String(text) => humantime::parse_duration(text).ok(),
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
                target: passthrough.target,
                delay,
            },
        }
    }
}

#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("failed to parse route yaml: {0}")]
    Yaml(#[from] serde_yaml::Error),
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
    #[error("route `{route}` has an invalid pipeline: {reason}")]
    InvalidPipeline { route: String, reason: String },
    #[error("behavior `{address}` is invalid: {reason}")]
    InvalidBehavior { address: String, reason: String },
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
            ("250", 250),
            ("'500ms'", 500),
            ("2s", 2000),
            ("1m 30s", 90_000),
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
    }
}
