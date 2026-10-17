use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Http,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Route {
    pub id: String,
    pub transport: Transport,
    #[serde(rename = "match")]
    pub route_match: HttpRouteMatch,
    #[serde(default)]
    pub cases: BTreeMap<String, Case>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passthrough: Option<PassthroughTarget>,
}

impl Route {
    pub fn from_yaml(input: &str) -> Result<Self, ValidationError> {
        let route: Self = serde_yaml::from_str(input)?;
        route.validate()?;
        Ok(route)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_id("route", &self.id)?;

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
                if let Behavior::Response(response) = behavior {
                    crate::template::validate_response(response).map_err(|reason| {
                        ValidationError::InvalidBehavior {
                            address: format!("{}:{case_id}:{behavior_id}", self.id),
                            reason,
                        }
                    })?;
                }
            }

            crate::matching::validate_case_match(&case.case_match).map_err(|reason| {
                ValidationError::InvalidMatch {
                    route: self.id.clone(),
                    case: case_id.clone(),
                    reason,
                }
            })?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpRouteMatch {
    pub method: HttpMethod,
    pub path: String,
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
                target: None,
                delay,
            },
            Behavior::Passthrough(passthrough) => Self {
                kind: Some(BehaviorKind::Passthrough),
                status: None,
                headers: BTreeMap::new(),
                body: None,
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
        assert_eq!(route.route_match.path, "/users/{id}");
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
