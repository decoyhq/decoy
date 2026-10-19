use std::cell::OnceCell;

use jmespath::Rcvar;
use prost_reflect::MethodDescriptor;
use serde_json::{Value, json};

use super::codec::{decode, pipeline};
use super::schema::find_method;
use super::{GrpcRequest, GrpcResponse, INVALID_ARGUMENT};
use crate::collections::Activation;
use crate::engine::{Candidate, Catalog, MissDiagnostic, Rejection, reject};
use crate::matching::{grpc_case_matches, grpc_case_mismatch};
use crate::schema::{Behavior, PipelineStep, ResponseBehavior, Route, RouteMatch};

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

impl Catalog {
    /// Resolves with the full explanation; serving uses `execute_grpc`, which skips it.
    pub fn resolve_grpc(
        &self,
        collection_id: &str,
        overrides: &[Activation],
        request: &GrpcRequest,
    ) -> GrpcResolution<'_> {
        self.resolve_grpc_with(collection_id, overrides, request, true)
    }

    /// Unexplained (`explain: false`) leaves `skipped` and a miss's `candidates` empty: a gRPC
    /// miss answers with its reason only.
    pub(super) fn resolve_grpc_with(
        &self,
        collection_id: &str,
        overrides: &[Activation],
        request: &GrpcRequest,
        explain: bool,
    ) -> GrpcResolution<'_> {
        let activations = match self.activations(collection_id, overrides) {
            Ok(activations) => activations,
            Err(reason) => {
                return GrpcResolution::Miss(MissDiagnostic {
                    collection: collection_id.to_owned(),
                    reason,
                    candidates: Vec::new(),
                });
            }
        };

        let mut candidates = explain.then(Vec::new);
        // Decoded once per call; only another Pipeline needs a fresh decode.
        let mut decoded: Option<(&[PipelineStep], Value, OnceCell<Rcvar>)> = None;
        // The first decode error, the answer when no other Route matches.
        let mut decode_failure = None;
        for activation in activations.iter().copied().rev() {
            let Some(route) = self.route(&activation.route) else {
                reject(&mut candidates, activation, || Rejection::MissingRoute);
                continue;
            };
            let RouteMatch::Grpc(address) = &route.route_match else {
                reject(&mut candidates, activation, || Rejection::Transport);
                continue;
            };
            if address.service != request.service || address.method != request.method {
                reject(&mut candidates, activation, || Rejection::Method {
                    expected: format!("{}/{}", address.service, address.method),
                });
                continue;
            }
            // Startup checks every gRPC Route against the protos, so this only guards the lookup.
            let Ok(method) = find_method(&self.runtime().protos, &address.service, &address.method)
            else {
                reject(&mut candidates, activation, || Rejection::MissingRoute);
                continue;
            };
            let steps = pipeline(route);
            let (envelope, jmes) = match &mut decoded {
                Some((cached, envelope, jmes)) if *cached == steps => (envelope, &*jmes),
                _ => {
                    let message = match decode(&request.body, steps, &method.input()) {
                        Ok(message) => message,
                        Err(error) => {
                            decode_failure.get_or_insert_with(|| {
                                format!(
                                    "route `{}` could not decode the request: {error}",
                                    route.id
                                )
                            });
                            reject(&mut candidates, activation, || Rejection::Decode { error });
                            continue;
                        }
                    };
                    let envelope = json!({
                        "service": request.service,
                        "method": request.method,
                        "metadata": request.metadata,
                        "message": message,
                    });
                    let (_, envelope, jmes) = decoded.insert((steps, envelope, OnceCell::new()));
                    (envelope, &*jmes)
                }
            };
            let Some(case) = route.cases.get(&activation.case) else {
                reject(&mut candidates, activation, || Rejection::MissingCase);
                continue;
            };
            if explain {
                if let Some(failure) = grpc_case_mismatch(&case.case_match, envelope, jmes) {
                    reject(&mut candidates, activation, || {
                        Rejection::Condition(failure)
                    });
                    continue;
                }
            } else if !grpc_case_matches(&case.case_match, envelope, jmes) {
                continue;
            }
            let Some(Behavior::Response(response)) = case.behaviors.get(&activation.behavior)
            else {
                reject(&mut candidates, activation, || Rejection::MissingBehavior);
                continue;
            };
            return GrpcResolution::Matched {
                activation: activation.clone(),
                skipped: candidates.unwrap_or_default(),
                conditions: &case.case_match,
                route,
                method,
                response,
                envelope: std::mem::take(envelope),
            };
        }

        let candidates = candidates.unwrap_or_default();
        match decode_failure {
            Some(reason) => GrpcResolution::Failed {
                response: GrpcResponse::error(INVALID_ARGUMENT, reason),
                candidates,
            },
            None => GrpcResolution::Miss(MissDiagnostic {
                collection: collection_id.to_owned(),
                reason: "no active route case matched request".to_owned(),
                candidates,
            }),
        }
    }
}
