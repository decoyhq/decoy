use crate::collections::Activation;

pub const SESSION_HEADER: &str = "x-mock-session";
pub const DEFAULT_SESSION: &str = "default";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub(crate) collection: String,
    /// `useCollection` picked `collection`; otherwise it follows the default across reloads.
    pub(super) collection_picked: bool,
    /// In call order; see [`Catalog::activations`](super::Catalog::activations) for how they apply.
    pub(crate) route_overrides: Vec<Activation>,
    /// [`Controller`](super::Controller)'s clock at the last verb that changed it; the lowest is evicted first.
    pub(super) touched: u64,
}

impl Selection {
    pub(super) fn new(collection: impl Into<String>) -> Self {
        Self {
            collection: collection.into(),
            collection_picked: false,
            route_overrides: Vec::new(),
            touched: 0,
        }
    }

    pub fn snapshot(&self) -> SelectionSnapshot {
        SelectionSnapshot {
            collection: self.collection.clone(),
            overrides: self
                .route_overrides
                .iter()
                .map(|activation| RouteOverrideSnapshot {
                    route: activation.route.clone(),
                    case_id: activation.case.clone(),
                    behavior: activation.behavior.clone(),
                })
                .collect(),
        }
    }

    /// A repeated `route+case` moves to the end: among appended overrides the last call wins.
    pub(super) fn use_route(&mut self, route: String, case: String, behavior: String) {
        self.route_overrides
            .retain(|o| o.route != route || o.case != case);
        self.route_overrides.push(Activation {
            route,
            case,
            behavior,
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SelectionSnapshot {
    pub collection: String,
    pub overrides: Vec<RouteOverrideSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RouteOverrideSnapshot {
    pub route: String,
    #[serde(rename = "case")]
    pub case_id: String,
    pub behavior: String,
}

impl RouteOverrideSnapshot {
    /// The override's Behavior address, `route:case:behavior`.
    pub fn address(&self) -> String {
        format!("{}:{}:{}", self.route, self.case_id, self.behavior)
    }
}
