use std::cell::OnceCell;
use std::collections::BTreeMap;

use serde_json::Value;

use super::resolve::WsResolution;
use super::{Trigger, WsConnection, WsReply, encode_text};
use crate::collections::Activation;
use crate::engine::Catalog;
use crate::events::{Match, Outcome};
use crate::expression::to_rcvar;
use crate::schema::ResponseBehavior;
use crate::template::render_ws_response;

/// An answered message or open, and what an event needs to explain it.
pub(crate) struct WsCall {
    /// `Ok(None)`: nothing matched. `Err`: the matched Behavior did not render.
    pub reply: Result<Option<WsReply>, String>,
    /// Only when `capture` was asked for.
    pub outcome: Option<Outcome>,
}

impl Catalog {
    pub(crate) fn execute_ws(
        &self,
        collection_id: &str,
        overrides: &[Activation],
        connection: &WsConnection,
        trigger: Trigger,
        envelope: &Value,
        capture: bool,
    ) -> WsCall {
        match self.resolve_ws_with(
            collection_id,
            overrides,
            &connection.route,
            trigger,
            envelope,
            capture,
        ) {
            WsResolution::Matched {
                activation,
                skipped,
                conditions,
                response,
            } => WsCall {
                reply: respond(response, envelope).map(Some).map_err(|reason| {
                    format!("behavior `{}` failed: {reason}", activation.address())
                }),
                outcome: capture.then(|| {
                    Outcome::Hit(Match {
                        path_params: path_params(envelope),
                        activation,
                        skipped,
                        conditions: conditions.clone(),
                    })
                }),
            },
            WsResolution::Miss(miss) => WsCall {
                reply: Ok(None),
                outcome: capture.then_some(Outcome::Miss(miss)),
            },
        }
    }
}

fn respond(response: &ResponseBehavior, envelope: &Value) -> Result<WsReply, String> {
    let jmes = OnceCell::new();
    let rendered = render_ws_response(response, &|| {
        jmes.get_or_init(|| to_rcvar(envelope)).clone()
    })
    .map_err(|error| error.to_string())?;
    let single = rendered
        .message
        .map(|message| (None, encode_text(&message)));
    let listed = rendered
        .messages
        .into_iter()
        .flatten()
        .map(|item| (item.delay, encode_text(&item.message)));
    Ok(WsReply {
        delay: rendered.delay,
        messages: single.into_iter().chain(listed).collect(),
        close: rendered.close,
    })
}

fn path_params(envelope: &Value) -> BTreeMap<String, String> {
    serde_json::from_value(envelope["pathParams"].clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::collections::CollectionsFile;
    use crate::schema::{Route, WsClose};

    fn catalog(behaviors: &str) -> Catalog {
        let route = Route::from_yaml(&format!(
            "id: live\ntransport: ws\nmatch: {{ path: /live }}\ncases:\n  any:\n    behaviors:\n{behaviors}"
        ))
        .unwrap();
        Catalog::new(
            vec![route],
            CollectionsFile::from_yaml("- id: default\n  routes: [live:any:b]\n").unwrap(),
            crate::http::RuntimeConfig::default(),
        )
        .unwrap()
    }

    fn reply(catalog: &Catalog, message: Value) -> Result<Option<WsReply>, String> {
        let connection = catalog
            .ws_upgrade("/live?topic=news", &BTreeMap::new())
            .unwrap();
        let envelope = connection.envelope(Some(&message));
        catalog
            .execute_ws(
                "default",
                &[],
                &connection,
                Trigger::Message,
                &envelope,
                false,
            )
            .reply
    }

    #[test]
    fn renders_messages_as_text_frames_with_their_delays_and_close() {
        let catalog = catalog(
            r#"      b:
        delay: 50ms
        messages:
          - { message: { id: '${ message.id }', topic: '${ query.topic }' } }
          - { message: 'bye ${ message.id }', delay: 1s }
        close: { code: 4001, reason: done }
"#,
        );
        assert_eq!(
            reply(&catalog, json!({ "id": 7 })).unwrap(),
            Some(WsReply {
                delay: Some(Duration::from_millis(50)),
                messages: vec![
                    (None, r#"{"id":7,"topic":"news"}"#.to_owned()),
                    (Some(Duration::from_secs(1)), "bye 7".to_owned()),
                ],
                close: Some(WsClose {
                    code: 4001,
                    reason: "done".to_owned()
                }),
            })
        );
    }

    #[test]
    fn a_behavior_that_does_not_render_names_itself() {
        let catalog = catalog("      b: { message: '${ abs(message.a) }' }\n");
        let error = reply(&catalog, json!({ "a": "x" })).unwrap_err();
        assert!(
            error.starts_with("behavior `live:any:b` failed:"),
            "{error}"
        );
    }
}
