//! The request detail panel's text: plain indented lines with stable labels, so a copied detail
//! reads well. Color only marks headings and verdicts, and `NO_COLOR` drops it.

use decoy_runtime::events::{CapturedBody, Match, Outcome, RequestEvent};
use decoy_runtime::schema::Transport;
use decoy_runtime::{Candidate, Rejection};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::log::Theme;

/// Label column width, so values line up (`passthrough`, `collection`).
const LABEL: usize = 11;
/// Rendered bytes kept per body; the rest is reported as `…truncated N bytes`.
pub const BODY_CAP: usize = 64 * 1024;

pub fn lines(record: &RequestEvent, theme: &Theme) -> Vec<Line<'static>> {
    let mut out = Out {
        lines: Vec::new(),
        theme,
    };

    out.heading("Request");
    out.field("transport", transport(&record.transport));
    out.field("method", &record.method);
    out.field("url", &record.url);
    out.field("session", &record.session);
    out.headers(record.headers.iter());
    out.body(&record.body);

    out.blank();
    out.heading("Match");
    match &record.outcome {
        Outcome::Hit(hit) => out.matched("hit", Color::Green, hit, record),
        Outcome::Passthrough { matched, .. } => {
            out.matched("passthrough", Color::Yellow, matched, record);
        }
        Outcome::Miss(miss) => {
            out.push(vec![
                Span::raw("  "),
                Span::styled(format!("{:<LABEL$}", "MISS"), out.verdict(Color::Red)),
                Span::raw(format!(" {}", miss.reason)),
            ]);
            out.field("collection", &miss.collection);
            out.candidates(&miss.candidates, record);
            if miss.candidates.is_empty() {
                out.text("    (none active)".to_owned());
            }
        }
    }

    out.blank();
    out.heading("Response");
    // The gRPC status code for a gRPC call, whose headers are its trailers.
    out.field("status", &record.response.status.to_string());
    if let Outcome::Passthrough {
        upstream_url,
        upstream_status,
        ..
    } = &record.outcome
    {
        out.field(
            "upstream",
            &format!(
                "{} → {}",
                upstream_url.as_deref().unwrap_or("<no target>"),
                upstream_status.map_or("failed".to_owned(), |status| status.to_string())
            ),
        );
    }
    out.headers(
        record
            .response
            .headers
            .iter()
            .map(|(name, value)| (name, value)),
    );
    out.body(&record.response.body);

    out.blank();
    out.heading("Timing");
    out.field("at", &(theme.clock)(record.at));
    out.field("duration", &format!("{:?}", record.duration));
    out.lines
}

fn transport(transport: &Transport) -> &'static str {
    match transport {
        Transport::Http => "http",
        Transport::Grpc => "grpc",
    }
}

struct Out<'a> {
    lines: Vec<Line<'static>>,
    theme: &'a Theme<'a>,
}

impl Out<'_> {
    fn push(&mut self, spans: Vec<Span<'static>>) {
        self.lines.push(Line::from(spans));
    }

    fn text(&mut self, text: String) {
        self.lines.push(Line::raw(text));
    }

    fn blank(&mut self) {
        self.lines.push(Line::default());
    }

    fn heading(&mut self, text: &str) {
        let style = self.theme.style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
        self.lines.push(Line::styled(text.to_owned(), style));
    }

    fn verdict(&self, color: Color) -> Style {
        self.theme
            .style(Style::default().fg(color).add_modifier(Modifier::BOLD))
    }

    /// `  label     value`, labels padded so values line up.
    fn field(&mut self, label: &str, value: &str) {
        self.text(format!("  {label:<LABEL$} {}", printable(value)));
    }

    fn headers<'h>(&mut self, headers: impl Iterator<Item = (&'h String, &'h String)>) {
        self.text("  headers".to_owned());
        let mut any = false;
        for (name, value) in headers {
            self.text(format!("    {name}: {}", printable(value)));
            any = true;
        }
        if !any {
            self.text("    (none)".to_owned());
        }
    }

    fn body(&mut self, body: &CapturedBody) {
        self.text(if body.decoded {
            "  body (protobuf as JSON)".to_owned()
        } else {
            "  body".to_owned()
        });
        for line in body_lines(body) {
            self.text(format!("    {line}"));
        }
    }

    fn matched(&mut self, verdict: &str, color: Color, hit: &Match, record: &RequestEvent) {
        self.push(vec![
            Span::raw("  "),
            Span::styled(format!("{verdict:<LABEL$}"), self.verdict(color)),
            Span::raw(format!(" {}", hit.activation.address())),
        ]);
        self.candidates(&hit.skipped, record);
        self.push(vec![
            Span::raw("    "),
            Span::styled("✓", self.verdict(Color::Green)),
            Span::raw(format!(" {}", hit.activation.address())),
        ]);
        self.field("passed", &compact(&hit.conditions));
        if !hit.path_params.is_empty() {
            let params = serde_json::to_string(&hit.path_params).expect("strings serialize");
            self.field("pathParams", &params);
        }
    }

    /// Every rejected candidate, in evaluation order (bottom-to-top), with why.
    fn candidates(&mut self, candidates: &[Candidate], record: &RequestEvent) {
        self.text("  candidates (evaluation order)".to_owned());
        for candidate in candidates {
            self.push(vec![
                Span::raw("    "),
                Span::styled("✗", self.verdict(Color::Red)),
                Span::raw(format!(" {}", candidate.address)),
            ]);
            self.text(format!(
                "        {}",
                rejection(&candidate.rejection, record)
            ));
        }
    }
}

fn rejection(rejection: &Rejection, record: &RequestEvent) -> String {
    match rejection {
        Rejection::MissingRoute => "route not in the catalog".to_owned(),
        Rejection::MissingCase => "case not in the route".to_owned(),
        Rejection::MissingBehavior => "behavior not in the case".to_owned(),
        Rejection::Transport => "route serves another transport".to_owned(),
        Rejection::Method { expected } => {
            format!("method: expected {expected}, got {}", record.method)
        }
        Rejection::Path { expected } => {
            let path = record.url.split('?').next().unwrap_or_default();
            format!("path: expected {expected}, got {path}")
        }
        Rejection::Condition(failure) => format!(
            "{}: expected {}, got {}",
            failure.field,
            compact(&failure.expected),
            compact(&failure.actual)
        ),
    }
}

fn compact(value: &serde_json::Value) -> String {
    if value.is_null() {
        return "(none)".to_owned();
    }
    printable(&value.to_string())
}

/// A body in its most-decoded form: JSON pretty-printed, other text as-is, else
/// `<binary, N bytes>`. At most [`BODY_CAP`] rendered bytes, then `…truncated N bytes`.
pub fn body_lines(body: &CapturedBody) -> Vec<String> {
    if body.size == 0 {
        return vec!["(empty)".to_owned()];
    }
    // Bytes the capture already left out (the runtime keeps the first 64KB).
    let mut cut = body.size.saturating_sub(body.bytes.len());
    let mut text = match serde_json::from_slice::<serde_json::Value>(&body.bytes) {
        Ok(json) => serde_json::to_string_pretty(&json).expect("JSON value serializes"),
        Err(_) => match std::str::from_utf8(&body.bytes) {
            Ok(text) => text.to_owned(),
            // A capture cut inside a multi-byte character is still text.
            Err(error) if error.error_len().is_none() => {
                cut += body.bytes.len() - error.valid_up_to();
                String::from_utf8_lossy(&body.bytes[..error.valid_up_to()]).into_owned()
            }
            Err(_) => return vec![format!("<binary, {} bytes>", body.size)],
        },
    };
    // NUL never shows up in text a person would read.
    if text.contains('\0') {
        return vec![format!("<binary, {} bytes>", body.size)];
    }
    if text.len() > BODY_CAP {
        let mut end = BODY_CAP;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        cut += text.len() - end;
        text.truncate(end);
    }
    let mut lines: Vec<String> = text.lines().map(printable).collect();
    if cut > 0 {
        lines.push(format!("…truncated {cut} bytes"));
    }
    lines
}

/// `text` with tabs expanded and other control characters replaced, so a body can't send
/// escape sequences to the terminal.
fn printable(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\t' => out.push_str("    "),
            c if c.is_control() => out.push(char::REPLACEMENT_CHARACTER),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(bytes: &[u8]) -> CapturedBody {
        CapturedBody::raw(bytes)
    }

    #[test]
    fn json_is_pretty_printed_and_text_kept() {
        assert_eq!(body_lines(&body(br#"{"a":1}"#)), ["{", r#"  "a": 1"#, "}"]);
        assert_eq!(body_lines(&body(b"hello\nworld")), ["hello", "world"]);
        assert_eq!(body_lines(&body(b"")), ["(empty)"]);
    }

    #[test]
    fn undecodable_bytes_show_their_size() {
        assert_eq!(
            body_lines(&body(b"\x89PNG\r\n\x1a\n\x00\xff")),
            ["<binary, 10 bytes>"]
        );
    }

    #[test]
    fn control_characters_are_not_sent_to_the_terminal() {
        assert_eq!(body_lines(&body(b"a\x1b[2Jb\tc")), ["a\u{fffd}[2Jb    c"]);
    }

    #[test]
    fn text_with_nul_bytes_is_binary() {
        assert_eq!(body_lines(&body(b"a\x00b")), ["<binary, 3 bytes>"]);
    }

    #[test]
    fn pretty_printing_past_the_cap_is_cut_too() {
        // 40KB of JSON grows past 64KB once each number gets its own indented line.
        let json = format!("[{}]", vec!["1"; 20_000].join(","));
        let pretty = serde_json::to_string_pretty(
            &serde_json::from_str::<serde_json::Value>(&json).unwrap(),
        )
        .unwrap();
        let lines = body_lines(&body(json.as_bytes()));
        let shown: usize = lines[..lines.len() - 1].iter().map(|l| l.len() + 1).sum();
        assert!(shown <= BODY_CAP + 1, "{shown}");
        assert_eq!(
            lines.last().unwrap(),
            &format!("…truncated {} bytes", pretty.len() - BODY_CAP)
        );
    }

    #[test]
    fn long_bodies_stop_at_the_cap_and_say_how_much_is_left() {
        let lines = body_lines(&body(&vec![b'a'; BODY_CAP + 100]));
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), BODY_CAP);
        assert_eq!(lines[1], "…truncated 100 bytes");
    }
}

#[cfg(test)]
mod snapshots {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use decoy_runtime::MissDiagnostic;
    use decoy_runtime::events::{CapturedBody, Outcome};
    use decoy_runtime::matching::ConditionFailure;
    use serde_json::json;

    use super::*;
    use crate::app::tests::{base, matched, utc_clock};

    fn text(record: &RequestEvent) -> String {
        let theme = Theme {
            session: "default",
            no_color: true,
            clock: utc_clock,
        };
        lines(record, &theme)
            .iter()
            .map(crate::log::plain)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn candidate(address: &str, rejection: Rejection) -> Candidate {
        Candidate {
            address: address.into(),
            rejection,
        }
    }

    #[test]
    fn a_hit_with_skipped_candidates() {
        let mut hit = matched("users-by-id:user-123:success");
        hit.skipped = vec![
            candidate(
                "users-by-id:admin:success",
                Rejection::Condition(ConditionFailure {
                    field: "headers.x-role".into(),
                    expected: json!("admin"),
                    actual: json!(null),
                }),
            ),
            candidate(
                "orders:any:ok",
                Rejection::Path {
                    expected: "/orders".into(),
                },
            ),
        ];
        hit.conditions = json!({ "pathParams": { "id": "123" } });
        hit.path_params = BTreeMap::from([("id".into(), "123".into())]);
        let mut record = base(41, Outcome::Hit(hit));
        record.url = "/users/123?expand=1".into();
        record.headers = BTreeMap::from([("accept".into(), "application/json".into())]);
        record.response.headers = vec![("content-type".into(), "application/json".into())];
        record.response.body = CapturedBody::raw(br#"{"id":"123","name":"Ada"}"#);
        insta::assert_snapshot!(text(&record));
    }

    #[test]
    fn a_miss_with_several_reasons() {
        let mut record = base(
            42,
            Outcome::Miss(MissDiagnostic {
                collection: "happy-path".into(),
                reason: "no active route case matched request".into(),
                candidates: vec![
                    candidate(
                        "orders:create:ok",
                        Rejection::Method {
                            expected: "POST".into(),
                        },
                    ),
                    candidate(
                        "users-by-id:user-1:ada",
                        Rejection::Condition(ConditionFailure {
                            field: "pathParams.id".into(),
                            expected: json!("1"),
                            actual: json!("999"),
                        }),
                    ),
                    candidate("flags:any:on", Rejection::MissingCase),
                ],
            }),
        );
        record.url = "/users/999".into();
        record.response.status = 501;
        record.response.body = CapturedBody::raw(br#"{"error":"mock miss"}"#);
        insta::assert_snapshot!(text(&record));
    }

    #[test]
    fn a_passthrough() {
        let mut record = base(
            43,
            Outcome::Passthrough {
                matched: matched("avatars:any:real"),
                upstream_url: Some("http://cdn.local/avatars/ada.png".into()),
                upstream_status: Some(200),
            },
        );
        record.url = "/avatars/ada.png".into();
        record.response.body = CapturedBody::raw(b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR");
        insta::assert_snapshot!(text(&record));
    }

    #[test]
    fn a_grpc_call_with_json_bodies() {
        let mut record = base(44, Outcome::Hit(matched("approvals-get:any:ok")));
        record.transport = Transport::Grpc;
        record.method = "POST".into();
        record.url = "/approvals.v1.Approvals/Get".into();
        record.headers = BTreeMap::from([("content-type".into(), "application/grpc".into())]);
        record.body = CapturedBody::decoded(&json!({ "id": "a-1" }));
        record.response.status = 0;
        record.response.body = CapturedBody::decoded(&json!({ "id": "a-1", "state": "APPROVED" }));
        insta::assert_snapshot!(text(&record));
    }

    #[test]
    fn a_binary_and_a_truncated_body() {
        let mut record = base(45, Outcome::Hit(matched("upload:any:ok")));
        record.method = "PUT".into();
        record.url = "/upload".into();
        record.body = CapturedBody::raw(&[0xff, 0xfe, 0x00, 0x01]);
        let mut big = br#"{"items":["#.to_vec();
        big.extend(std::iter::repeat_n(
            b'x',
            decoy_runtime::events::BODY_CAP + 10,
        ));
        record.response.body = CapturedBody::raw(&big);
        record.duration = Duration::from_micros(1500);
        let text = text(&record);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.contains(&"    <binary, 4 bytes>"), "{text}");
        assert!(
            lines.contains(
                &format!(
                    "    …truncated {} bytes",
                    big.len() - decoy_runtime::events::BODY_CAP
                )
                .as_str()
            ),
            "{text}"
        );
        // Everything but the 64KB body line, so the snapshot stays readable.
        let short: Vec<&str> = lines.into_iter().filter(|line| line.len() < 200).collect();
        insta::assert_snapshot!(short.join("\n"));
    }
}
