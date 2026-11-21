//! Decoy native runtime prototype.
//!
//! This crate starts with the portable route/case/behavior schema. The current
//! TypeScript implementation is prior art, not the semantic source of truth.

pub mod collections;
pub mod config;
pub mod connections;
pub mod control_api;
pub mod engine;
pub mod events;
pub mod expression;
pub mod grpc;
pub mod http;
pub mod matching;
pub mod plugins;
pub mod protos;
pub mod request;
pub mod schema;
pub mod server;
pub mod startup;
pub mod template;
mod time;
pub mod tls;
pub mod watch;
pub mod ws;
mod yaml;

pub use collections::{Activation, Collection, CollectionRouteRef, CollectionsFile};
pub use control_api::{
    CONTROL_PREFIX, ControlApiRequest, ControlApiResponse, DEFAULT_SESSION, SEND_PATH,
    SESSION_HEADER, WAIT_PATH, handle_control_request, handle_send_request, handle_wait_request,
    try_handle_control_request,
};
pub use engine::{
    Candidate, Catalog, ControlError, Controller, MissDiagnostic, Rejection, RouteOverrideSnapshot,
    Selection, SelectionSnapshot,
};
pub use http::forward::{ForwardRequest, ForwardResponse, ForwardingError, forward_passthrough};
pub use http::{
    BodyPlan, HttpExecutionOutcome, HttpExecutionRequest, HttpRequest, HttpResponsePlan,
    PassthroughPlan, RequestMetadata, ResolveOutcome, ResponsePlan, RuntimeConfig,
};
pub use request::RequestEnvelope;
pub use schema::{
    Behavior, BehaviorKind, Case, GrpcRouteMatch, HttpMethod, HttpRouteMatch, Route, RouteMatch,
    Transport, ValidationError, WsRouteMatch,
};
pub use server::{BoundListener, Reloaded, Reloader, ServeError, Server, serve};
pub use startup::{
    SourceLocation, Startup, StartupDiagnostic, StartupDiagnosticKind, StartupError,
    load_catalog_from_files,
};
