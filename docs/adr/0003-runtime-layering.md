# 0003. Runtime layering: transport-neutral engine, transport folders, edge adapters

Date: 2026-10-19

## Status

Accepted. The move to this layout lands as a series of behaviour-preserving PRs (#267).

## Context

`decoy-runtime` grew file by file. `engine.rs` mixes pure resolution, I/O (delays, passthrough),
wire conversion, Session state, and event capture. `grpc.rs` and `serve.rs` have the same mix. The
core imports constants from the Control API, and adding WebSocket and SSE (#218) would make every
file bigger still.

## Decision

- **Functional core, imperative shell, per transport.** Resolving a request is pure: activations
  plus a request in, a plan (Matched or Miss) out. Executing a plan is the thin I/O shell: delay,
  forward, encode.
- **Layout of `crates/decoy-runtime/src`:**
  - `engine/`: transport-neutral core (Catalog, Session, Selection, Controller, SessionView).
  - `http/`, `grpc/` (later `ws/`, `sse/`): each transport owns its resolve, execute and wire code.
    `http/forward.rs` is the outbound passthrough adapter.
  - `server/` (axum ingress, lifecycle, reload) and `control_api.rs`: inbound adapters.
  - Loading and model (`schema`, `config`, `startup`, `collections`, `matching`, `expression`,
    `template`) stay flat.
- **Dependency rule.** `engine/` never imports `server`, `control_api`, transport folders' I/O, axum,
  hyper, or reqwest. Transport folders never import the Controller. Adapters depend inward only.
- **Typed errors where an error crosses a layer or drives a decision**, via `thiserror`, one enum
  per module. Load-time validation reasons that only become diagnostic text stay `String` on
  purpose. `Display` text is part of the wire contract (miss bodies, Control API `{error}`), so
  changing a message is a behaviour change.
- **No speculative abstraction.** No `Transport` trait, no traits for single-implementation
  dependencies (forwarder, clock, event sink), no extra crates. Extract a shared helper only when a
  third copy appears.

## Consequences

- New transports add a folder; `engine/` does not change shape.
- The core can be unit-tested without a server or network.
- Moves happen in their own PRs (`git mv` first, no logic edits, no test assertion changes) so
  review and blame stay readable.
- Rules for agents and contributors live in `crates/decoy-runtime/AGENTS.md`.
