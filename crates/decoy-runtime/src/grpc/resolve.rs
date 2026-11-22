use std::cell::OnceCell;
use std::collections::HashMap;

use jmespath::Rcvar;
use prost_reflect::MethodDescriptor;
use serde_json::{Value, json};

use super::codec::{decode, pipeline};
use super::schema::find_method;
use super::{GrpcRequest, GrpcResponse};
use crate::collections::Activation;
use crate::engine::{Candidate, Catalog, MissDiagnostic, Rejection, reject};
use crate::matching::{envelope_case_matches, envelope_case_mismatch};
use crate::schema::{Behavior, ResponseBehavior, Route, RouteMatch};

/// Which active route entry answers a unary call, and why the others did not.
#[derive(Debug)]
pub enum GrpcResolution<'a> {
    Matched {
        activation: Activation,
        /// Candidates rejected before the winner, in evaluation order; empty when serving.
        skipped: Vec<Candidate>,
        /// The winning Case's `match`; every condition in it passed.
        conditions: &'a Value,
        route: &'a Route,
        method: MethodDescriptor,
        response: &'a ResponseBehavior,
        envelope: Value,
    },
    Miss(MissDiagnostic),
    /// Nothing matched and a Route could not decode the request: `INVALID_ARGUMENT`.
    Failed {
        response: GrpcResponse,
        candidates: Vec<Candidate>,
    },
}

pub(super) struct PendingDecode<'catalog, 'entry> {
    pub route: &'catalog Route,
    pub activation: &'entry Activation,
    pub method: MethodDescriptor,
}

impl Catalog {
    /// Resolves legacy built-in Pipelines synchronously. Named Pipelines can call external
    /// Transformers and require `resolve_grpc_async`.
    pub fn resolve_grpc(
        &self,
        collection_id: &str,
        overrides: &[Activation],
        request: &GrpcRequest,
    ) -> GrpcResolution<'_> {
        use futures_util::FutureExt as _;
        self.select_grpc(collection_id, overrides, request, true, false)
            .now_or_never()
            .expect("built-in Pipelines are synchronous")
    }

    /// Match the remaining entries up to the next named input Pipeline. The driver resumes at
    /// that entry after the transform completes, so earlier Cases are never evaluated twice.
    pub(super) fn grpc_candidates<'catalog, 'entry>(
        &'catalog self,
        activations: impl Iterator<Item = &'entry Activation>,
        request: &GrpcRequest,
        transformed: &HashMap<&str, Result<Value, String>>,
        candidates: &mut Option<Vec<Candidate>>,
        decode_failure: &mut Option<String>,
    ) -> Result<Option<GrpcResolution<'catalog>>, PendingDecode<'catalog, 'entry>> {
        // Decoded once per call, failure included; only another Pipeline needs a fresh decode.
        type Decoded = Result<(Value, OnceCell<Rcvar>), String>;
        let mut decoded: Option<(&Route, Decoded)> = None;
        for activation in activations {
            let Some(route) = self.route(&activation.route) else {
                reject(candidates, activation, || Rejection::MissingRoute);
                continue;
            };
            let RouteMatch::Grpc(address) = &route.route_match else {
                reject(candidates, activation, || Rejection::Transport);
                continue;
            };
            if address.service != request.service || address.method != request.method {
                reject(candidates, activation, || Rejection::Method {
                    expected: format!("{}/{}", address.service, address.method),
                });
                continue;
            }
            // Startup checks every gRPC Route against the protos, so this only guards the lookup.
            let Ok(method) = find_method(&self.runtime().protos, &address.service, &address.method)
            else {
                reject(candidates, activation, || Rejection::MissingRoute);
                continue;
            };
            let has_named_input = route
                .pipeline_name
                .as_ref()
                .is_some_and(|name| !self.runtime().pipelines[name].input.is_empty());
            if !matches!(&decoded, Some((cached, _)) if cached.id == route.id ||
                (cached.pipeline_name.is_none() && route.pipeline_name.is_none() && pipeline(cached) == pipeline(route)))
            {
                let message = if has_named_input {
                    transformed
                        .get(route.id.as_str())
                        .ok_or_else(|| PendingDecode {
                            route,
                            activation,
                            method: method.clone(),
                        })?
                        .clone()
                } else {
                    decode(&request.body, pipeline(route), &method.input())
                        .map_err(|error| error.to_string())
                };
                let result = message.map(|message| {
                    let envelope = json!({
                        "service": request.service,
                        "method": request.method,
                        "metadata": request.metadata,
                        "message": message,
                    });
                    (envelope, OnceCell::new())
                });
                decoded = Some((route, result));
            }
            let Some((_, result)) = &mut decoded else {
                unreachable!("decoded just above")
            };
            let (envelope, jmes) = match result {
                Ok((envelope, jmes)) => (envelope, &*jmes),
                Err(error) => {
                    // Named Pipeline errors here are explicit input misses. Hard failures
                    // already ended the call in the execute driver.
                    if !has_named_input {
                        decode_failure.get_or_insert_with(|| {
                            format!("route `{}` could not decode the request: {error}", route.id)
                        });
                    }
                    reject(candidates, activation, || Rejection::Decode {
                        error: error.clone(),
                    });
                    continue;
                }
            };
            let Some(case) = route.cases.get(&activation.case) else {
                reject(candidates, activation, || Rejection::MissingCase);
                continue;
            };
            if candidates.is_some() {
                if let Some(failure) = envelope_case_mismatch(&case.case_match, envelope, jmes) {
                    reject(candidates, activation, || Rejection::Condition(failure));
                    continue;
                }
            } else if !envelope_case_matches(&case.case_match, envelope, jmes) {
                continue;
            }
            let Some(Behavior::Response(response)) = case.behaviors.get(&activation.behavior)
            else {
                reject(candidates, activation, || Rejection::MissingBehavior);
                continue;
            };
            return Ok(Some(GrpcResolution::Matched {
                activation: activation.clone(),
                skipped: candidates.take().unwrap_or_default(),
                conditions: &case.case_match,
                route,
                method,
                response,
                envelope: std::mem::take(envelope),
            }));
        }

        Ok(None)
    }
}
