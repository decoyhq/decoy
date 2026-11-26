use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use thiserror::Error;

use super::ControlError;
use crate::collections::{Activation, CollectionError, CollectionsFile};
use crate::http::{BodyPlan, HttpResponsePlan, RuntimeConfig};
use crate::schema::{Behavior, Route, RouteMatch};
use crate::template::has_templates;

pub(crate) type ResolvedCollections =
    BTreeMap<String, Result<Vec<Activation>, Arc<CollectionError>>>;

#[derive(Debug, Clone)]
pub struct Catalog {
    pub(crate) routes: BTreeMap<String, Route>,
    collections: CollectionsFile,
    /// Each Collection resolved once (inheritance, validation) instead of on every request.
    resolved: ResolvedCollections,
    /// HTTP Response Behaviors without templates, planned once with the body serialized:
    /// route → case → behavior.
    pub(crate) static_plans: BTreeMap<String, BTreeMap<String, BTreeMap<String, HttpResponsePlan>>>,
    pub(crate) grpc_replies: HashMap<Activation, crate::grpc::GrpcResponse>,
    pub(crate) runtime: RuntimeConfig,
}

impl Catalog {
    pub fn new(
        routes: Vec<Route>,
        collections: CollectionsFile,
        runtime: RuntimeConfig,
    ) -> Result<Self, CatalogError> {
        for (index, route) in routes.iter().enumerate() {
            route.validate()?;
            crate::pipeline::Pipeline::validate_route(route, &runtime)
                .map_err(|reason| CatalogError::Pipeline { reason })?;
            crate::grpc::schema::check_configured_route(route, &runtime)
                .map_err(|reason| CatalogError::GrpcRoute { reason })?;
            if let Some(other) = crate::ws::schema::find_tie(&routes[..index], route) {
                return Err(CatalogError::WsRoute {
                    reason: crate::ws::schema::tie_error(route, other),
                });
            }
        }
        let resolved = collections
            .ids()
            .map(|id| (id.to_owned(), collections.resolve(id).map_err(Arc::new)))
            .collect();
        Self::from_checked(routes, collections, resolved, runtime)
    }

    /// Assemble a Catalog from checked Routes and already resolved Collections.
    pub(crate) fn from_checked(
        routes: Vec<Route>,
        collections: CollectionsFile,
        resolved: ResolvedCollections,
        runtime: RuntimeConfig,
    ) -> Result<Self, CatalogError> {
        let mut by_id = BTreeMap::new();

        for route in routes {
            if by_id.insert(route.id.clone(), route).is_some() {
                return Err(CatalogError::DuplicateRoute);
            }
        }

        let static_plans = by_id
            .values()
            .filter(|route| matches!(route.route_match, RouteMatch::Http(_)))
            .map(|route| {
                let transforms_output = route
                    .pipeline_name
                    .as_ref()
                    .and_then(|name| runtime.pipelines.get(name))
                    .is_some_and(|pipeline| !pipeline.output.is_empty());
                let cases = route.cases.iter().map(|(case_id, case)| {
                    let behaviors = case.behaviors.iter().filter_map(|(name, behavior)| {
                        let Behavior::Response(response) = behavior else {
                            return None;
                        };
                        if has_templates(response) {
                            return None;
                        }
                        let mut plan = HttpResponsePlan::from_response(response);
                        if !transforms_output {
                            plan.body = plan.body.map(|body| BodyPlan::Bytes(body.into_bytes()));
                        }
                        Some((name.clone(), plan))
                    });
                    (case_id.clone(), behaviors.collect())
                });
                (route.id.clone(), cases.collect())
            })
            .collect();
        let grpc_replies = crate::grpc::static_unary_replies(by_id.values(), &runtime.protos);
        Ok(Self {
            grpc_replies,
            routes: by_id,
            collections,
            resolved,
            static_plans,
            runtime,
        })
    }

    /// Every Route, by id.
    pub fn routes(&self) -> impl Iterator<Item = &Route> {
        self.routes.values()
    }

    pub fn collection_ids(&self) -> impl Iterator<Item = &str> {
        self.collections.ids()
    }

    pub fn validate_collection(&self, collection_id: &str) -> Result<(), ControlError> {
        self.collections
            .get(collection_id)
            .map(|_| ())
            .ok_or_else(|| ControlError::UnknownCollection {
                id: collection_id.to_owned(),
            })
    }

    pub fn validate_behavior_address(
        &self,
        route_id: &str,
        case_id: &str,
        behavior_id: &str,
    ) -> Result<(), ControlError> {
        let route = self
            .routes
            .get(route_id)
            .ok_or_else(|| ControlError::UnknownRoute {
                route: route_id.to_owned(),
            })?;
        let case = route
            .cases
            .get(case_id)
            .ok_or_else(|| ControlError::UnknownCase {
                route: route_id.to_owned(),
                case: case_id.to_owned(),
            })?;
        case.behaviors
            .get(behavior_id)
            .map(|_| ())
            .ok_or_else(|| ControlError::UnknownBehavior {
                route: route_id.to_owned(),
                case: case_id.to_owned(),
                behavior: behavior_id.to_owned(),
            })
    }

    /// The Collection's activations with Session overrides applied: an override replaces its
    /// `route+case` entry in place, or is appended after the Collection in call order.
    pub(crate) fn activations<'a>(
        &'a self,
        collection_id: &str,
        overrides: &'a [Activation],
    ) -> Result<impl DoubleEndedIterator<Item = &'a Activation>, Arc<CollectionError>> {
        let collection = self
            .resolved
            .get(collection_id)
            .ok_or_else(|| {
                Arc::new(CollectionError::UnknownCollection {
                    id: collection_id.to_owned(),
                })
            })?
            .as_ref()
            .map_err(Clone::clone)?;
        let same = |a: &Activation, b: &Activation| a.route == b.route && a.case == b.case;
        let entries = collection
            .iter()
            .map(move |entry| overrides.iter().find(|o| same(o, entry)).unwrap_or(entry));
        let appended = overrides
            .iter()
            .filter(move |o| !collection.iter().any(|entry| same(o, entry)));
        Ok(entries.chain(appended))
    }

    pub(crate) fn route(&self, id: &str) -> Option<&Route> {
        self.routes.get(id)
    }

    pub(crate) fn runtime(&self) -> &RuntimeConfig {
        &self.runtime
    }
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error(transparent)]
    Validation(#[from] crate::schema::ValidationError),
    #[error("duplicate route id")]
    DuplicateRoute,
    #[error("{reason}")]
    Pipeline { reason: String },
    /// A gRPC Route that doesn't fit the loaded protos.
    #[error("{reason}")]
    GrpcRoute { reason: String },
    /// WebSocket Routes that tie on an upgrade (Q12).
    #[error("{reason}")]
    WsRoute { reason: String },
    #[error(transparent)]
    Collection(#[from] CollectionError),
}
