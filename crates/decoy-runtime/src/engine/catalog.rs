use std::collections::BTreeMap;

use thiserror::Error;

use super::ControlError;
use crate::collections::{Activation, CollectionError, CollectionsFile};
use crate::http::{BodyPlan, HttpResponsePlan, RequestMetadata, ResponsePlan, RuntimeConfig};
use crate::schema::{Behavior, Route, RouteMatch};
use crate::template::has_templates;

#[derive(Debug, Clone, PartialEq)]
pub struct Catalog {
    pub(crate) routes: BTreeMap<String, Route>,
    collections: CollectionsFile,
    /// Each Collection resolved once (inheritance, validation) instead of on every request.
    resolved: BTreeMap<String, Result<Vec<Activation>, String>>,
    /// HTTP Response Behaviors without templates, planned once with the body serialized:
    /// route → case → behavior.
    pub(crate) static_plans: BTreeMap<String, BTreeMap<String, BTreeMap<String, HttpResponsePlan>>>,
    pub(crate) runtime: RuntimeConfig,
}

impl Catalog {
    pub fn new(
        routes: Vec<Route>,
        collections: CollectionsFile,
        runtime: RuntimeConfig,
    ) -> Result<Self, CatalogError> {
        for route in &routes {
            route.validate()?;
            crate::grpc::schema::check_route(route, &runtime.protos).map_err(CatalogError::Grpc)?;
        }
        Self::from_checked(routes, collections, runtime)
    }

    /// `new` for Routes startup already validated and checked against the protos (with file
    /// paths in its diagnostics), so the expensive proto check runs once.
    pub(crate) fn from_checked(
        routes: Vec<Route>,
        collections: CollectionsFile,
        runtime: RuntimeConfig,
    ) -> Result<Self, CatalogError> {
        let mut by_id = BTreeMap::new();

        for route in routes {
            if by_id.insert(route.id.clone(), route).is_some() {
                return Err(CatalogError::DuplicateRoute);
            }
        }

        let resolved = collections
            .ids()
            .map(|id| {
                let activations = collections.resolve(id).map_err(|error| error.to_string());
                (id.to_owned(), activations)
            })
            .collect();
        let static_plans = by_id
            .values()
            .filter(|route| matches!(route.route_match, RouteMatch::Http(_)))
            .map(|route| {
                let cases = route.cases.iter().map(|(case_id, case)| {
                    let behaviors = case.behaviors.iter().filter_map(|(name, behavior)| {
                        let Behavior::Response(response) = behavior else {
                            return None;
                        };
                        if has_templates(response) {
                            return None;
                        }
                        let metadata = RequestMetadata::default();
                        let ResponsePlan::Response(mut plan) =
                            ResponsePlan::from_behavior(route, behavior, &runtime, &metadata)
                        else {
                            unreachable!("a Response Behavior plans a response")
                        };
                        plan.body = plan.body.map(|body| BodyPlan::Bytes(body.into_bytes()));
                        Some((name.clone(), plan))
                    });
                    (case_id.clone(), behaviors.collect())
                });
                (route.id.clone(), cases.collect())
            })
            .collect();
        Ok(Self {
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
    ) -> Result<Vec<&'a Activation>, String> {
        let collection = self
            .resolved
            .get(collection_id)
            .ok_or_else(|| {
                CollectionError::UnknownCollection {
                    id: collection_id.to_owned(),
                }
                .to_string()
            })?
            .as_ref()
            .map_err(Clone::clone)?;
        let same = |a: &Activation, b: &Activation| a.route == b.route && a.case == b.case;
        let mut activations: Vec<&Activation> = collection
            .iter()
            .map(|entry| overrides.iter().find(|o| same(o, entry)).unwrap_or(entry))
            .collect();
        activations.extend(
            overrides
                .iter()
                .filter(|o| !collection.iter().any(|entry| same(o, entry))),
        );
        Ok(activations)
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
    #[error("{0}")]
    Grpc(String),
    #[error(transparent)]
    Collection(#[from] CollectionError),
}
