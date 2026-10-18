//! Decoy native runtime prototype.
//!
//! This crate starts with the portable route/case/behavior schema. The current
//! TypeScript implementation is prior art, not the semantic source of truth.

pub mod collections;
pub mod config;
pub mod control_api;
pub mod engine;
pub mod events;
pub mod expression;
pub mod grpc;
pub mod grpc_schema;
pub mod http;
pub mod http_forward;
pub mod matching;
pub mod protos;
pub mod request;
pub mod schema;
pub mod serve;
pub mod startup;
pub mod template;
pub mod tls;
pub mod watch;

pub use collections::{Activation, Collection, CollectionRouteRef, CollectionsFile};
pub use control_api::{
    CONTROL_PREFIX, ControlApiRequest, ControlApiResponse, DEFAULT_SESSION, SESSION_HEADER,
    handle_control_request, try_handle_control_request,
};
pub use engine::{
    Candidate, Catalog, ControlError, Controller, HttpExecutionOutcome, HttpExecutionRequest,
    HttpRequest, MissDiagnostic, Rejection, ResolveOutcome, RouteOverrideSnapshot, Selection,
    SelectionSnapshot,
};
pub use http::{
    BodyPlan, HttpResponsePlan, PassthroughPlan, RequestMetadata, ResponsePlan, RuntimeConfig,
};
pub use http_forward::{ForwardRequest, ForwardResponse, ForwardingError, forward_passthrough};
pub use request::RequestEnvelope;
pub use schema::{
    Behavior, BehaviorKind, Case, GrpcRouteMatch, HttpMethod, HttpRouteMatch, Route, RouteMatch,
    Transport, ValidationError,
};
pub use serve::{BoundListener, Reloaded, Reloader, ServeError, Server, serve};
pub use startup::{
    SourceLocation, Startup, StartupDiagnostic, StartupDiagnosticKind, StartupError,
    load_catalog_from_files,
};
