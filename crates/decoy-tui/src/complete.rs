//! Completion candidates for the input line: command names, then each command's argument.
//! `/route` completes one segment at a time (route → case → behavior) from the live catalog.

use decoy_runtime::Catalog;

use crate::command::COMMANDS;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// Byte offset in the input; accepting replaces everything from here on with `insert`.
    pub from: usize,
    pub insert: String,
    pub label: String,
    pub about: String,
    /// Accepting it leaves a whole command, so Enter also runs it.
    pub done: bool,
}

/// What argument completion can draw from besides the catalog.
pub struct Sources<'a> {
    pub catalog: &'a Catalog,
    /// Sessions seen so far, sorted.
    pub sessions: Vec<&'a str>,
    pub session: &'a str,
    pub collection: &'a str,
}

pub fn complete(input: &str, sources: &Sources) -> Vec<Completion> {
    if !input.starts_with('/') {
        return Vec::new();
    }
    let Some((name, arg)) = input.split_once(' ') else {
        return COMMANDS
            .iter()
            .filter(|spec| spec.name.starts_with(input))
            .map(|spec| Completion {
                from: 0,
                insert: if spec.args.is_empty() {
                    spec.name.to_owned()
                } else {
                    format!("{} ", spec.name)
                },
                label: spec.name.to_owned(),
                about: spec.about.to_owned(),
                done: spec.args.is_empty(),
            })
            .collect();
    };
    let from = name.len() + 1;
    let ids = |ids: Vec<&str>, active: &str| -> Vec<Completion> {
        candidates(ids, arg, true)
            .into_iter()
            .map(|id| Completion {
                from,
                insert: id.to_owned(),
                label: id.to_owned(),
                about: if id == active { "active" } else { "" }.to_owned(),
                done: true,
            })
            .collect()
    };
    match name {
        "/collection" => ids(
            sources.catalog.collection_ids().collect(),
            sources.collection,
        ),
        "/session" => ids(sources.sessions.clone(), sources.session),
        "/route" => route(arg, from, sources.catalog),
        _ => Vec::new(),
    }
}

fn route(arg: &str, from: usize, catalog: &Catalog) -> Vec<Completion> {
    let segments: Vec<&str> = arg.split(':').collect();
    let partial = segments[segments.len() - 1];
    let from = from + arg.len() - partial.len();
    let find = |id: &str| catalog.routes().find(|route| route.id == id);
    let (ids, about, last): (Vec<&str>, &str, bool) = match segments[..] {
        [_] => (
            catalog.routes().map(|route| route.id.as_str()).collect(),
            "route",
            false,
        ),
        [route, _] => match find(route) {
            Some(route) => (
                route.cases.keys().map(String::as_str).collect(),
                "case",
                false,
            ),
            None => return Vec::new(),
        },
        [route, case, _] => match find(route).and_then(|route| route.cases.get(case)) {
            Some(case) => (
                case.behaviors.keys().map(String::as_str).collect(),
                "behavior",
                true,
            ),
            None => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    candidates(ids, partial, last)
        .into_iter()
        .map(|id| Completion {
            from,
            insert: if last {
                id.to_owned()
            } else {
                format!("{id}:")
            },
            label: id.to_owned(),
            about: about.to_owned(),
            done: last,
        })
        .collect()
}

/// The sorted `ids` that start with `typed`, so an exact match comes first and Enter keeps it.
/// When `typed` is already the only match of a `whole` argument there is nothing to offer.
fn candidates<'a>(ids: Vec<&'a str>, typed: &str, whole: bool) -> Vec<&'a str> {
    let matching: Vec<&str> = ids.into_iter().filter(|id| id.starts_with(typed)).collect();
    if whole && matching == [typed] {
        return Vec::new();
    }
    matching
}

#[cfg(test)]
pub(crate) mod tests {
    use decoy_runtime::{CollectionsFile, Route, RuntimeConfig};

    use super::*;

    pub fn catalog() -> Catalog {
        let users = "id: users-by-id\ntransport: http\nmatch: { method: GET, path: '/users/{id}' }\n\
            cases:\n  user-1:\n    match: { pathParams: { id: '1' } }\n    behaviors:\n      ada: { status: 200 }\n      missing: { status: 404 }\n\
            \x20 any:\n    behaviors:\n      ok: { status: 200 }\n";
        let orders = "id: orders\ntransport: http\nmatch: { method: GET, path: /orders }\n\
            cases:\n  any:\n    behaviors:\n      ok: { status: 200 }\n";
        Catalog::new(
            vec![
                Route::from_yaml(users).unwrap(),
                Route::from_yaml(orders).unwrap(),
            ],
            CollectionsFile::from_yaml(
                "- id: happy-path\n  routes: [users-by-id:user-1:ada]\n- id: errors\n  routes: [users-by-id:user-1:missing]\n",
            )
            .unwrap(),
            RuntimeConfig::default(),
        )
        .unwrap()
    }

    fn labels(input: &str) -> Vec<(String, String, bool)> {
        let catalog = catalog();
        let sources = Sources {
            catalog: &catalog,
            sessions: vec!["default", "e2e-1"],
            session: "default",
            collection: "happy-path",
        };
        complete(input, &sources)
            .into_iter()
            .map(|c| (input[..c.from].to_owned() + &c.insert, c.about, c.done))
            .collect()
    }

    fn texts(input: &str) -> Vec<String> {
        labels(input).into_iter().map(|(text, ..)| text).collect()
    }

    #[test]
    fn command_names_complete_with_a_space_when_they_take_an_argument() {
        assert_eq!(texts("/s"), ["/session ", "/sessions", "/status", "/show "]);
        assert!(labels("/re")[0].2);
        assert_eq!(texts("/r"), ["/route ", "/reset"]);
        assert!(texts("hello").is_empty());
    }

    #[test]
    fn route_completes_one_segment_at_a_time() {
        assert_eq!(texts("/route "), ["/route orders:", "/route users-by-id:"]);
        assert_eq!(texts("/route us"), ["/route users-by-id:"]);
        assert_eq!(
            texts("/route users-by-id:"),
            ["/route users-by-id:any:", "/route users-by-id:user-1:"]
        );
        assert_eq!(
            labels("/route users-by-id:user-1:"),
            [
                (
                    "/route users-by-id:user-1:ada".into(),
                    "behavior".into(),
                    true
                ),
                (
                    "/route users-by-id:user-1:missing".into(),
                    "behavior".into(),
                    true
                ),
            ]
        );
        assert_eq!(
            texts("/route users-by-id:user-1:m"),
            ["/route users-by-id:user-1:missing"]
        );
        assert!(texts("/route users-by-id:user-1:missing").is_empty());
        assert!(texts("/route nope:").is_empty());
        assert!(texts("/route users-by-id:nope:").is_empty());
    }

    #[test]
    fn an_exact_id_stays_first_when_a_longer_one_shares_its_prefix() {
        let catalog = catalog();
        let sources = Sources {
            catalog: &catalog,
            sessions: vec!["e2e", "e2e-1"],
            session: "default",
            collection: "happy-path",
        };
        let first = |input: &str| complete(input, &sources).remove(0).insert;
        assert_eq!(first("/session e2e"), "e2e");
        assert_eq!(first("/session e2"), "e2e");
        assert!(complete("/session e2e-1", &sources).is_empty());
    }

    #[test]
    fn collections_and_sessions_mark_the_active_one() {
        assert_eq!(
            labels("/collection "),
            [
                ("/collection errors".into(), String::new(), true),
                ("/collection happy-path".into(), "active".into(), true),
            ]
        );
        assert_eq!(texts("/session e"), ["/session e2e-1"]);
        assert_eq!(labels("/session ")[0].1, "active");
    }
}
