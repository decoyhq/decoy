use std::collections::BTreeMap;
use std::sync::Arc;

use thiserror::Error;

use super::{Catalog, DEFAULT_SESSION, Selection, SelectionSnapshot, SessionView};
use crate::collections::Activation;
use crate::events::{Event, SessionChange, SessionEvent, SessionSource};
#[cfg(test)]
use crate::http::RequestMetadata;
#[cfg(test)]
use crate::http::{HttpRequest, ResolveOutcome};

#[derive(Debug, Error)]
pub enum ControlError {
    #[error("unknown collection `{id}`")]
    UnknownCollection { id: String },
    #[error("unknown route `{route}`")]
    UnknownRoute { route: String },
    #[error("unknown case `{route}:{case}`")]
    UnknownCase { route: String, case: String },
    #[error("unknown behavior `{route}:{case}:{behavior}`")]
    UnknownBehavior {
        route: String,
        case: String,
        behavior: String,
    },
}

/// How many Sessions hold a Selection at once before the least recently changed one is ended.
pub const MAX_SESSIONS: usize = 10_000;

#[derive(Debug, Clone)]
pub struct Controller {
    /// Immutable after startup; requests share it instead of copying Routes and Collections.
    catalog: Arc<Catalog>,
    default_collection: String,
    sessions: BTreeMap<String, Selection>,
    /// Counts verbs that change a Selection, to find the Session changed least recently.
    clock: u64,
    events: crate::events::Sender,
    /// Who is calling the verbs; [`Controller::with_source`] sets it for one call.
    source: SessionSource,
}

impl Controller {
    pub fn new(catalog: Catalog, default_collection: impl Into<String>) -> Self {
        Self {
            catalog: Arc::new(catalog),
            default_collection: default_collection.into(),
            sessions: BTreeMap::new(),
            clock: 0,
            events: crate::events::channel(),
            source: SessionSource::InProcess,
        }
    }

    /// Every [`RequestEvent`](crate::events::RequestEvent) and [`SessionEvent`] from now on.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub(crate) fn events(&self) -> crate::events::Sender {
        self.events.clone()
    }

    /// Runs `verbs` with every [`SessionEvent`] they emit tagged `source`, then restores the
    /// previous source.
    pub(crate) fn with_source<T>(
        &mut self,
        source: SessionSource,
        verbs: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let previous = std::mem::replace(&mut self.source, source);
        let result = verbs(self);
        self.source = previous;
        result
    }

    #[cfg(test)]
    pub fn use_collection(&mut self, session: &str, collection: impl Into<String>) {
        let selection = self.selection_mut(session);
        selection.collection = collection.into();
        selection.collection_picked = true;
        self.emit(session, SessionChange::UseCollection);
    }

    pub fn try_use_collection(
        &mut self,
        session: &str,
        collection: impl Into<String>,
    ) -> Result<SelectionSnapshot, ControlError> {
        let collection = collection.into();
        self.catalog.validate_collection(&collection)?;
        let selection = self.selection_mut(session);
        selection.collection = collection;
        selection.collection_picked = true;
        self.emit(session, SessionChange::UseCollection);
        Ok(self.selection(session).snapshot())
    }

    /// Skips address validation; tests use it to plant overrides the Catalog lacks.
    #[cfg(test)]
    pub fn use_route(
        &mut self,
        session: &str,
        route: impl Into<String>,
        case: impl Into<String>,
        behavior: impl Into<String>,
    ) {
        self.selection_mut(session)
            .use_route(route.into(), case.into(), behavior.into());
        self.emit(session, SessionChange::UseRoute);
    }

    pub fn try_use_route(
        &mut self,
        session: &str,
        route: impl Into<String>,
        case: impl Into<String>,
        behavior: impl Into<String>,
    ) -> Result<SelectionSnapshot, ControlError> {
        let route = route.into();
        let case = case.into();
        let behavior = behavior.into();
        self.catalog
            .validate_behavior_address(&route, &case, &behavior)?;
        self.selection_mut(session).use_route(route, case, behavior);
        self.emit(session, SessionChange::UseRoute);
        Ok(self.selection(session).snapshot())
    }

    pub fn reset(&mut self, session: &str) -> SelectionSnapshot {
        self.selection_mut(session).route_overrides.clear();
        self.emit(session, SessionChange::Reset);
        self.selection(session).snapshot()
    }

    /// Forgets the Session's Selection, freeing it; a later request starts from the defaults.
    pub fn end_session(&mut self, session: &str) -> SelectionSnapshot {
        if self.sessions.remove(session).is_some() {
            self.emit(session, SessionChange::Ended);
        }
        self.selection(session).snapshot()
    }

    /// Serves `catalog` from now on; requests already running finish against the old one.
    /// Selections are kept: a Collection that is gone falls back to `default_collection`, a
    /// Session that never picked one follows the new default, and overrides whose Behavior
    /// address no longer exists are dropped. Returns one warning per fallback or drop.
    pub fn reload(
        &mut self,
        catalog: Catalog,
        default_collection: impl Into<String>,
    ) -> Vec<String> {
        self.catalog = Arc::new(catalog);
        self.default_collection = default_collection.into();
        let mut warnings = Vec::new();
        let mut changed = Vec::new();
        for (session, selection) in &mut self.sessions {
            let before = selection.clone();
            if !selection.collection_picked {
                selection.collection.clone_from(&self.default_collection);
            } else if self
                .catalog
                .validate_collection(&selection.collection)
                .is_err()
            {
                warnings.push(format!(
                    "session {session}: collection `{}` is gone, back to `{}`",
                    selection.collection, self.default_collection
                ));
                selection.collection.clone_from(&self.default_collection);
                selection.collection_picked = false;
            }
            selection.route_overrides.retain(|Activation { route, case, behavior }| {
                let exists = self
                    .catalog
                    .validate_behavior_address(route, case, behavior)
                    .is_ok();
                if !exists {
                    warnings.push(format!(
                        "session {session}: override `{route}:{case}:{behavior}` dropped, it no longer exists"
                    ));
                }
                exists
            });
            if *selection != before {
                self.clock += 1;
                selection.touched = self.clock;
                changed.push(session.clone());
            }
        }
        for session in changed {
            self.emit(&session, SessionChange::Reload);
        }
        warnings
    }

    /// What a Session that never picked a Collection uses.
    pub fn default_collection(&self) -> &str {
        &self.default_collection
    }

    /// The Routes and Collections this Controller serves, for read-only inspection.
    pub fn catalog(&self) -> Arc<Catalog> {
        Arc::clone(&self.catalog)
    }

    /// The Session's Selection; the defaults for a Session that holds none.
    pub fn selection_snapshot(&self, session: &str) -> SelectionSnapshot {
        self.selection(session).snapshot()
    }

    #[cfg(test)]
    pub fn resolve_http(&self, session: &str, request: &HttpRequest) -> ResolveOutcome<'_> {
        let selection = self.selection(session);
        self.catalog.resolve_http_with_overrides(
            &selection.collection,
            request,
            &selection.route_overrides,
            &RequestMetadata::default(),
            true,
        )
    }

    pub fn session_view(&self, session: &str) -> SessionView {
        SessionView {
            catalog: Arc::clone(&self.catalog),
            session: session.to_owned(),
            selection: self.selection(session),
            events: self.events.clone(),
        }
    }

    fn selection(&self, session: &str) -> Selection {
        self.sessions
            .get(session)
            .cloned()
            .unwrap_or_else(|| Selection::new(self.default_collection.clone()))
    }

    fn selection_mut(&mut self, session: &str) -> &mut Selection {
        if !self.sessions.contains_key(session) {
            self.evict_if_full();
            let selection = Selection::new(self.default_collection.clone());
            self.sessions.insert(session.to_owned(), selection);
            self.emit(session, SessionChange::Created);
        }
        self.clock += 1;
        let selection = self.sessions.get_mut(session).expect("inserted above");
        selection.touched = self.clock;
        selection
    }

    /// At [`MAX_SESSIONS`], ends the Session changed least recently, as `endSession` would, so a
    /// client that never calls `endSession` can't grow the map forever. The default Session stays.
    fn evict_if_full(&mut self) {
        if self.sessions.len() < MAX_SESSIONS {
            return;
        }
        let Some(oldest) = self
            .sessions
            .iter()
            .filter(|(session, _)| session.as_str() != DEFAULT_SESSION)
            .min_by_key(|(_, selection)| selection.touched)
            .map(|(session, _)| session.clone())
        else {
            return;
        };
        crate::events::warn(format!(
            "session {oldest} ended: {MAX_SESSIONS} sessions held, and it changed least recently"
        ));
        self.end_session(&oldest);
    }

    fn emit(&self, session: &str, change: SessionChange) {
        if self.events.receiver_count() > 0 {
            let _ = self.events.send(Event::Session(SessionEvent {
                session: session.to_owned(),
                change,
                source: self.source,
                selection: self.selection(session).snapshot(),
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::engine::test_support::{catalog, explain_catalog};
    use crate::schema::HttpMethod;

    #[test]
    fn golden_per_session_selection_is_isolated() {
        let mut controller = Controller::new(
            catalog(
                r#"
- id: local
  routes:
    - get-user:user-123:success

- id: not-found
  routes:
    - get-user:user-123:missing
"#,
            ),
            "local",
        );

        controller.use_collection("session-a", "not-found");

        let request = HttpRequest {
            method: HttpMethod::Get,
            path: "/users/123".to_owned(),
            ..Default::default()
        };

        let ResolveOutcome::Matched {
            activation: session_a,
            ..
        } = controller.resolve_http("session-a", &request)
        else {
            panic!("expected session-a match")
        };
        let ResolveOutcome::Matched {
            activation: session_b,
            ..
        } = controller.resolve_http("session-b", &request)
        else {
            panic!("expected session-b match")
        };

        assert_eq!(session_a.address(), "get-user:user-123:missing");
        assert_eq!(session_b.address(), "get-user:user-123:success");
    }

    #[test]
    fn an_embedded_controller_broadcasts_its_session_changes() {
        let mut controller = Controller::new(explain_catalog(), "local");
        let mut events = controller.subscribe();

        controller.use_collection("s1", "local");

        let changes: Vec<_> = std::iter::from_fn(|| match events.try_recv() {
            Ok(Event::Session(event)) => Some((event.session, event.change, event.source)),
            _ => None,
        })
        .collect();
        assert_eq!(
            changes,
            [
                (
                    "s1".to_owned(),
                    SessionChange::Created,
                    SessionSource::InProcess
                ),
                (
                    "s1".to_owned(),
                    SessionChange::UseCollection,
                    SessionSource::InProcess
                ),
            ]
        );
    }

    #[test]
    fn golden_control_verbs_use_collection_use_route_and_reset() {
        let mut controller = Controller::new(
            catalog(
                r#"
- id: local
  routes:
    - get-user:user-123:success

- id: not-found
  routes:
    - get-user:user-123:missing
"#,
            ),
            "local",
        );
        let request = HttpRequest {
            method: HttpMethod::Get,
            path: "/users/123".to_owned(),
            ..Default::default()
        };

        controller.use_collection("session", "not-found");
        let ResolveOutcome::Matched { activation, .. } =
            controller.resolve_http("session", &request)
        else {
            panic!("expected useCollection match")
        };
        assert_eq!(activation.address(), "get-user:user-123:missing");

        controller.use_route("session", "get-user", "user-123", "success");
        let ResolveOutcome::Matched { activation, .. } =
            controller.resolve_http("session", &request)
        else {
            panic!("expected useRoute match")
        };
        assert_eq!(activation.address(), "get-user:user-123:success");

        controller.reset("session");
        let ResolveOutcome::Matched { activation, .. } =
            controller.resolve_http("session", &request)
        else {
            panic!("expected reset match")
        };
        assert_eq!(activation.address(), "get-user:user-123:missing");
    }

    fn matched_address(controller: &Controller, path: &str) -> String {
        let request = HttpRequest {
            method: HttpMethod::Get,
            path: path.to_owned(),
            ..Default::default()
        };
        let ResolveOutcome::Matched { activation, .. } = controller.resolve_http("s", &request)
        else {
            panic!("expected a match for {path}")
        };
        activation.address()
    }

    #[test]
    fn use_route_replaces_the_collection_entry_in_place() {
        let mut controller = Controller::new(
            catalog(
                r#"
- id: local
  routes:
    - get-user:any:fallback
    - get-user:user-123:missing
"#,
            ),
            "local",
        );

        controller.use_route("s", "get-user", "any", "fallback");

        // The more specific entry below it in the Collection still wins.
        assert_eq!(
            matched_address(&controller, "/users/123"),
            "get-user:user-123:missing"
        );
        assert_eq!(
            matched_address(&controller, "/users/7"),
            "get-user:any:fallback"
        );
    }

    #[test]
    fn past_the_cap_the_session_changed_least_recently_ends() {
        let mut controller = Controller::new(catalog("- id: local\n  routes: []\n"), "local");
        controller.reset(DEFAULT_SESSION);
        for i in 0..MAX_SESSIONS - 1 {
            controller.reset(&format!("s{i}"));
        }
        // Changed again, so `s1` is now the one changed least recently (the default never goes).
        controller.reset("s0");
        let mut events = controller.subscribe();

        controller.reset("new");

        assert_eq!(controller.sessions.len(), MAX_SESSIONS);
        assert!(!controller.sessions.contains_key("s1"));
        for kept in [DEFAULT_SESSION, "s0", "s2", "new"] {
            assert!(controller.sessions.contains_key(kept), "{kept}");
        }
        let Ok(Event::Session(ended)) = events.try_recv() else {
            panic!("expected a session event")
        };
        assert_eq!(
            (ended.session.as_str(), ended.change),
            ("s1", SessionChange::Ended)
        );
    }

    #[test]
    fn a_session_reload_changed_is_not_the_one_ended_past_the_cap() {
        let both = "- id: local\n  routes: []\n- id: other\n  routes: []\n";
        let mut controller = Controller::new(catalog(both), "local");
        controller.reset(DEFAULT_SESSION);
        controller.use_collection("s0", "other");
        for i in 1..MAX_SESSIONS - 1 {
            controller.reset(&format!("s{i}"));
        }
        // `other` is gone, so reload moves `s0` back to `local`: that counts as a change.
        controller.reload(catalog("- id: local\n  routes: []\n"), "local");

        controller.reset("new");

        assert!(controller.sessions.contains_key("s0"));
        assert!(!controller.sessions.contains_key("s1"));
    }

    #[test]
    fn use_route_outside_the_collection_appends_and_the_last_call_wins() {
        let mut controller = Controller::new(
            catalog(
                r#"
- id: local
  routes: []
"#,
            ),
            "local",
        );

        controller.use_route("s", "get-user", "user-123", "success");
        assert_eq!(
            matched_address(&controller, "/users/123"),
            "get-user:user-123:success"
        );

        controller.use_route("s", "get-user", "any", "fallback");
        assert_eq!(
            matched_address(&controller, "/users/123"),
            "get-user:any:fallback"
        );

        controller.use_route("s", "get-user", "user-123", "missing");
        assert_eq!(
            matched_address(&controller, "/users/123"),
            "get-user:user-123:missing"
        );
    }
}
