//! Load-time checks for WebSocket Routes (`docs/design/transports.md` §6).

use crate::http::path::split_path;
use crate::matching::ANY;
use crate::schema::{ResponseBehavior, Route, RouteMatch};
use crate::template::validate_value;

/// Shape of a WebSocket Behavior: `message` or `messages`, and an optional `close`. No HTTP or gRPC
/// fields.
pub fn validate_response(response: &ResponseBehavior) -> Result<(), String> {
    if response.status.is_some()
        || !response.headers.is_empty()
        || response.body.is_some()
        || !response.metadata.is_empty()
        || response.stream.is_some()
    {
        return Err("WebSocket Behaviors use `message`, `messages` and `close`".to_owned());
    }
    if response.message.is_some() && response.messages.is_some() {
        return Err("use `message` or `messages`, not both".to_owned());
    }
    response.message.iter().try_for_each(validate_value)?;
    for (index, item) in response.messages.iter().flatten().enumerate() {
        validate_value(&item.message).map_err(|reason| format!("`messages[{index}]`: {reason}"))?;
    }
    if let Some(close) = &response.close {
        if close.code != 1000 && !(3000..=4999).contains(&close.code) {
            return Err(format!(
                "close code {} must be 1000 or an application code 3000–4999",
                close.code
            ));
        }
        // A close frame holds 125 bytes: two for the code, the rest for the reason.
        if close.reason.len() > 123 {
            return Err("close reason must be at most 123 bytes".to_owned());
        }
    }
    Ok(())
}

/// An earlier WebSocket Route that would tie with `route` on some upgrade (Q12): the same path,
/// as many `query` keys, and no key both require with different values.
pub fn find_tie<'a>(earlier: &'a [Route], route: &Route) -> Option<&'a Route> {
    let RouteMatch::Ws(address) = &route.route_match else {
        return None;
    };
    earlier.iter().find(|other| {
        let RouteMatch::Ws(other) = &other.route_match else {
            return false;
        };
        paths_overlap(&other.path, &address.path)
            && other.query.len() == address.query.len()
            && other.query.iter().all(|(key, value)| {
                address
                    .query
                    .get(key)
                    .is_none_or(|mine| mine == value || mine == ANY || value == ANY)
            })
    })
}

/// Whether some path matches both templates: as many segments, each equal or a `{param}` in one.
fn paths_overlap(a: &str, b: &str) -> bool {
    let (a, b) = (split_path(a), split_path(b));
    let param = |segment: &str| segment.starts_with('{');
    a.len() == b.len()
        && a.iter()
            .zip(&b)
            .all(|(a, b)| a == b || param(a) || param(b))
}

/// The startup message for a tie.
pub fn tie_error(route: &Route, other: &Route) -> String {
    format!(
        "WebSocket Routes `{}` and `{}` can both match the same upgrade; give one more `query` keys or a different value",
        other.id, route.id
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(id: &str, query: &str) -> Route {
        Route::from_yaml(&format!(
            "id: {id}\ntransport: ws\nmatch: {{ path: /subscribe, query: {query} }}\ncases:\n  any:\n    behaviors:\n      quiet: {{ messages: [] }}\n"
        ))
        .unwrap()
    }

    #[test]
    fn routes_tie_only_when_one_upgrade_could_match_both_equally() {
        let friend = ws("friend", "{ service: friend }");
        let routes = [friend];
        assert!(find_tie(&routes, &ws("best", "{ service: best }")).is_none());
        assert!(find_tie(&routes, &ws("more", "{ service: friend, v: '2' }")).is_none());
        assert_eq!(
            find_tie(&routes, &ws("same", "{ service: friend }")).map(|r| r.id.as_str()),
            Some("friend")
        );
        assert!(find_tie(&routes, &ws("other-key", "{ client: web }")).is_some());
        assert!(find_tie(&routes, &ws("any", "{ service: '*' }")).is_some());
    }

    #[test]
    fn paths_overlap_when_one_upgrade_path_matches_both() {
        assert!(paths_overlap("/rooms/{room}", "/rooms/{id}"));
        assert!(paths_overlap("/rooms/{room}", "/rooms/lobby"));
        assert!(paths_overlap("/subscribe", "/subscribe/"));
        assert!(!paths_overlap("/rooms/{room}", "/rooms/{room}/chat"));
        assert!(!paths_overlap("/rooms/lobby", "/rooms/hall"));
    }

    #[test]
    fn behaviors_take_only_websocket_fields() {
        let behavior = |yaml: &str| {
            Route::from_yaml(&format!(
                "id: s\ntransport: ws\nmatch: {{ path: /s }}\ncases:\n  any:\n    behaviors:\n      b: {yaml}\n"
            ))
            .map_err(|error| error.to_string())
        };
        assert!(behavior("{ message: ping }").is_ok());
        assert!(
            behavior("{ messages: [{ message: { a: 1 }, delay: 1s }], close: { code: 4001 } }")
                .is_ok()
        );
        assert!(
            behavior("{ body: x }")
                .unwrap_err()
                .contains("use `message`, `messages`")
        );
        assert!(
            behavior("{ message: a, messages: [] }")
                .unwrap_err()
                .contains("not both")
        );
        assert!(
            behavior("{ close: { code: 1006 } }")
                .unwrap_err()
                .contains("3000–4999")
        );
        assert!(behavior("{ message: '${ [ }' }").is_err());
    }

    #[test]
    fn on_connect_is_one_matchless_case_of_a_websocket_route() {
        let route = |transport: &str, cases: &str| {
            let address = match transport {
                "ws" => "{ path: /s }",
                _ => "{ method: GET, path: /s }",
            };
            Route::from_yaml(&format!(
                "id: s\ntransport: {transport}\nmatch: {address}\ncases:\n{cases}"
            ))
            .map_err(|error| error.to_string())
        };
        let hello = "  hello:\n    on: connect\n    behaviors:\n      ping: { message: ping }\n";
        assert!(route("ws", hello).is_ok());
        assert!(
            route("http", hello)
                .unwrap_err()
                .contains("invalid `on`: it is for WebSocket Routes")
        );
        let with_match = "  hello:\n    on: connect\n    match: { message: x }\n    behaviors:\n      ping: { message: ping }\n";
        assert!(
            route("ws", with_match)
                .unwrap_err()
                .contains("takes no `match`")
        );
        let twice = format!("{hello}{}", hello.replace("hello", "again"));
        assert!(
            route("ws", &twice)
                .unwrap_err()
                .contains("already this Route's `on: connect`")
        );
    }
}
