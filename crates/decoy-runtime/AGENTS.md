# decoy-runtime

Layering rules for this crate. Background and rationale: `docs/adr/0003-runtime-layering.md`.
The layout below is the target; code still in flat files moves there in behaviour-preserving PRs.

## Where code goes

| layer            | path                                                                               | owns                                                                  |
| ---------------- | ---------------------------------------------------------------------------------- | --------------------------------------------------------------------- |
| core             | `engine/`                                                                          | Catalog, Session, Selection, Controller, SessionView                  |
| transport        | `http/`, `grpc/`, `ws/` (later `sse/`)                                             | resolve (pure), execute (I/O shell), wire codec                       |
| outbound adapter | `http/forward.rs`                                                                  | passthrough to a real upstream                                        |
| inbound adapter  | `server/`, `control_api.rs`                                                        | axum ingress, lifecycle, reload, Control API dispatch                 |
| load and model   | `schema`, `config`, `startup`, `collections`, `matching`, `expression`, `template` | decoy.yaml, Route files, Cases, JMESPath                              |
| shared (flat)    | `events`, `request`, `protos`, `tls`, `watch`, `connections`                       | event capture, request envelope, protos, TLS, watch, live connections |

## Rules

- `engine/` must not import `server`, `control_api`, axum, hyper, or reqwest, nor a transport's
  `execute` or `forward` code. It may import transport plan and request types (`ResponsePlan`,
  `HttpResponsePlan`, `BodyPlan`, `ResolveOutcome`, `HttpRequest`, `RequestMetadata`,
  `RuntimeConfig`, gRPC equivalents). Session constants (`DEFAULT_SESSION`, `SESSION_HEADER`) live
  in the core.
- Session-level execute drivers (`SessionView::execute_http`, `execute_grpc`) live in the transport
  folder (`http/view.rs`, `grpc/view.rs`) as `impl SessionView` blocks; `engine/view.rs` keeps the
  struct and event capture (`start`, `emit`).
- Transport folders must not import the Controller.
- Resolve is pure: no sleeps, no network, no clock. I/O belongs in execute or adapters.
- A new transport is a new folder. Don't add a `Transport` trait, traits for single
  implementations, or new crates.
- Errors: one `thiserror` enum per module where the error crosses a layer or is matched on.
  Load-time validation reasons may stay `String`. No `anyhow` in this crate.
- `Display` text of errors reaches miss bodies and the Control API `{error}` envelope. Changing it is
  a behaviour change; `crates/decoy/tests/serve_cli.rs`, `crates/decoy/tests/grpc_cli.rs` and
  `crates/decoy-runtime/tests/events.rs` are the guard.
- Keep public paths re-exported from `lib.rs` stable; `decoy` and `decoy-tui` use them.
- Move code in its own PR: `git mv` first, no logic edits, no test assertion changes.
