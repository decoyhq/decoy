use std::cell::OnceCell;

use jmespath::Rcvar;
use serde_json::Value;

use super::{MESSAGE_MISS, Trigger, WsConnection, upgrade_envelope};
use crate::collections::Activation;
use crate::engine::{Candidate, Catalog, MissDiagnostic, Rejection, reject};
use crate::http::path::match_path;
use crate::matching::{ANY, envelope_case_matches, envelope_case_mismatch};
use crate::request::RequestEnvelope;
use crate::schema::{Behavior, CaseTrigger, ResponseBehavior, RouteMatch};

/// Which active entry of the connection's Route answers a message or the open.
#[derive(Debug)]
pub enum WsResolution<'a> {
    Matched {
        activation: Activation,
        /// Candidates rejected before the winner, in evaluation order; empty when serving.
        skipped: Vec<Candidate>,
        /// The winning Case's `match`; every condition in it passed.
        conditions: &'a Value,
        response: &'a ResponseBehavior,
    },
    Miss(MissDiagnostic),
}

impl Catalog {
    /// The `ws` Route for an upgrade to `path_and_query`: its path matches and the upgrade has
    /// every `query` param it lists. Among several, the one listing more `query` keys wins;
    /// startup rejects ties (Q12), and a tie only a repeated query key creates gets `None`.
    pub fn ws_upgrade(
        &self,
        path_and_query: &str,
        headers: &std::collections::BTreeMap<String, String>,
    ) -> Option<WsConnection> {
        let request = OnceCell::new();
        let request = || {
            request.get_or_init(|| {
                RequestEnvelope::from_parts(None, path_and_query, headers, bytes::Bytes::new())
            })
        };
        let path = path_and_query
            .split_once('?')
            .map_or(path_and_query, |(path, _)| path);
        let candidates: Vec<_> = self
            .routes()
            .filter_map(|route| {
                let RouteMatch::Ws(address) = &route.route_match else {
                    return None;
                };
                let has_query = address.query.iter().all(|(key, expected)| {
                    request().query.get(key).is_some_and(|values| {
                        expected == ANY || values.iter().any(|value| value == expected)
                    })
                });
                if !has_query {
                    return None;
                }
                Some((route, address.query.len(), match_path(&address.path, path)?))
            })
            .collect();
        let best = candidates.iter().map(|(_, keys, _)| *keys).max()?;
        let mut winners = candidates.into_iter().filter(|(_, keys, _)| *keys == best);
        let (route, _, path_params) = winners.next()?;
        // A repeated query key (`?service=a&service=b`) can still tie Routes startup let through;
        // refuse rather than pick one by id.
        if winners.next().is_some() {
            return None;
        }
        let request = request();
        Some(WsConnection {
            route: route.id.clone(),
            url: path_and_query.to_owned(),
            headers: request.headers().clone(),
            envelope: upgrade_envelope(path, &path_params, request),
        })
    }

    /// Resolves with the full explanation; serving passes `explain: false`.
    pub fn resolve_ws(
        &self,
        collection_id: &str,
        overrides: &[Activation],
        route_id: &str,
        trigger: Trigger,
        envelope: &Value,
    ) -> WsResolution<'_> {
        self.resolve_ws_with(collection_id, overrides, route_id, trigger, envelope, true)
    }

    /// Only the connection's Route's entries are candidates; entries of other Routes are not
    /// listed, since an open socket never moves to another Route.
    pub(crate) fn resolve_ws_with(
        &self,
        collection_id: &str,
        overrides: &[Activation],
        route_id: &str,
        trigger: Trigger,
        envelope: &Value,
        explain: bool,
    ) -> WsResolution<'_> {
        let miss = |reason: String, candidates| {
            WsResolution::Miss(MissDiagnostic {
                collection: collection_id.to_owned(),
                reason,
                candidates,
            })
        };
        let activations = match self.activations(collection_id, overrides) {
            Ok(activations) => activations,
            Err(error) => return miss(error.to_string(), Vec::new()),
        };
        let Some(route) = self.route(route_id) else {
            return miss(format!("route `{route_id}` no longer exists"), Vec::new());
        };
        let mut candidates = explain.then(Vec::new);
        let jmes = OnceCell::<Rcvar>::new();
        for activation in activations.rev() {
            if activation.route != route_id {
                continue;
            }
            let Some(case) = route.cases.get(&activation.case) else {
                reject(&mut candidates, activation, || Rejection::MissingCase);
                continue;
            };
            let on_connect = case.on == Some(CaseTrigger::Connect);
            if on_connect != (trigger == Trigger::Connect) {
                continue;
            }
            if explain {
                if let Some(failure) = envelope_case_mismatch(&case.case_match, envelope, &jmes) {
                    reject(&mut candidates, activation, || {
                        Rejection::Condition(failure)
                    });
                    continue;
                }
            } else if !envelope_case_matches(&case.case_match, envelope, &jmes) {
                continue;
            }
            let Some(Behavior::Response(response)) = case.behaviors.get(&activation.behavior)
            else {
                reject(&mut candidates, activation, || Rejection::MissingBehavior);
                continue;
            };
            return WsResolution::Matched {
                activation: activation.clone(),
                skipped: candidates.unwrap_or_default(),
                conditions: &case.case_match,
                response,
            };
        }
        let reason = match trigger {
            Trigger::Connect => "no active `on: connect` Case",
            Trigger::Message => MESSAGE_MISS,
        };
        miss(reason.to_owned(), candidates.unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::collections::CollectionsFile;
    use crate::schema::Route;

    fn catalog(collection: &str) -> Catalog {
        let friend = Route::from_yaml(
            r#"
id: friend
transport: ws
match: { path: /subscribe, query: { service: friend } }
cases:
  hello:
    on: connect
    behaviors:
      ping: { message: ping }
  status:
    match: { message: { type: get-status } }
    behaviors:
      online: { message: { id: '${ message.id }', status: online } }
  any:
    behaviors:
      echo: { message: '${ message }' }
"#,
        )
        .unwrap();
        let best = Route::from_yaml(
            "id: best\ntransport: ws\nmatch: { path: /subscribe, query: { service: best } }\ncases:\n  any:\n    behaviors:\n      quiet: { messages: [] }\n",
        )
        .unwrap();
        let plain = Route::from_yaml(
            "id: plain\ntransport: ws\nmatch: { path: '/rooms/{room}' }\ncases:\n  any:\n    behaviors:\n      quiet: { messages: [] }\n",
        )
        .unwrap();
        Catalog::new(
            vec![friend, best, plain],
            CollectionsFile::from_yaml(&format!("- id: default\n  routes: [{collection}]\n"))
                .unwrap(),
            crate::http::RuntimeConfig::default(),
        )
        .unwrap()
    }

    #[test]
    fn an_upgrade_picks_the_route_by_path_and_query() {
        let catalog = catalog("friend:status:online");
        let headers = BTreeMap::from([("Cookie".to_owned(), "id=7".to_owned())]);
        let friend = catalog
            .ws_upgrade("/subscribe?client=web&service=friend", &headers)
            .unwrap();
        assert_eq!(friend.route, "friend");
        assert_eq!(
            friend.envelope["query"],
            json!({ "client": "web", "service": "friend" })
        );
        assert_eq!(friend.envelope["cookies"], json!({ "id": "7" }));
        assert!(
            catalog
                .ws_upgrade("/subscribe?service=nobody", &headers)
                .is_none()
        );
        for both in [
            "/subscribe?service=friend&service=best",
            "/subscribe?service=best&service=friend",
        ] {
            assert!(catalog.ws_upgrade(both, &headers).is_none(), "{both}");
        }

        let room = catalog.ws_upgrade("/rooms/a%20b", &headers).unwrap();
        assert_eq!(room.route, "plain");
        assert_eq!(room.envelope["pathParams"], json!({ "room": "a b" }));
    }

    #[test]
    fn upgrades_without_query_conditions_keep_all_connection_metadata() {
        let route = catalog("plain:any:quiet").route("plain").unwrap().clone();
        let catalog = Catalog::new(
            vec![route],
            CollectionsFile::from_yaml("- id: default\n  routes: [plain:any:quiet]\n").unwrap(),
            crate::http::RuntimeConfig::default(),
        )
        .unwrap();
        let headers = BTreeMap::from([("Cookie".to_owned(), "id=7".to_owned())]);
        assert!(catalog.ws_upgrade("/other?q=one", &headers).is_none());
        let connection = catalog
            .ws_upgrade("/rooms/a%2Fb?q=one&q=two", &headers)
            .unwrap();
        assert_eq!(connection.headers["cookie"], "id=7");
        assert_eq!(connection.envelope["cookies"], json!({"id": "7"}));
        assert_eq!(connection.envelope["query"], json!({"q": ["one", "two"]}));
        assert_eq!(connection.envelope["pathParams"], json!({"room": "a/b"}));
    }

    #[test]
    fn messages_match_message_cases_and_the_open_matches_on_connect() {
        let catalog = catalog("friend:hello:ping, friend:status:online");
        let connection = catalog
            .ws_upgrade("/subscribe?service=friend", &BTreeMap::new())
            .unwrap();
        let resolve = |trigger, message: Option<Value>| match catalog.resolve_ws(
            "default",
            &[],
            "friend",
            trigger,
            &connection.envelope(message.as_ref()),
        ) {
            WsResolution::Matched { activation, .. } => Ok(activation.address()),
            WsResolution::Miss(miss) => Err(miss),
        };

        assert_eq!(
            resolve(Trigger::Connect, None).unwrap(),
            "friend:hello:ping"
        );
        assert_eq!(
            resolve(Trigger::Message, Some(json!({ "type": "get-status" }))).unwrap(),
            "friend:status:online"
        );
        let miss = resolve(Trigger::Message, Some(json!({ "type": "nope" }))).unwrap_err();
        assert_eq!(miss.reason, "no active route case matched message");
        assert_eq!(
            miss.candidates.len(),
            1,
            "the on: connect entry is not a candidate"
        );
        assert_eq!(miss.candidates[0].address, "friend:status:online");
    }

    #[test]
    fn without_an_active_on_connect_case_the_open_misses() {
        let catalog = catalog("friend:status:online");
        let WsResolution::Miss(miss) =
            catalog.resolve_ws("default", &[], "friend", Trigger::Connect, &json!({}))
        else {
            panic!("expected a miss")
        };
        assert_eq!(miss.reason, "no active `on: connect` Case");
        assert!(miss.candidates.is_empty());
    }
}
