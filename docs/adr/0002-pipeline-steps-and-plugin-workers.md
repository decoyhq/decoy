# 0002. One ordered pipeline, JMESPath logic, plugin workers

Date: 2026-10-17

## Status

Accepted for direction. Hosting details may change once the first plugin worker is prototyped.
Amended by [ADR 0004](0004-built-in-steps-only-for-grpc.md): only `grpc-frame` and `protobuf` are
built in, a plugin is declared by its `command` alone (no manifest), and the plugin worker contract
comes before any further built-in step.

## Context

Decoy must mock HTTP, gRPC, WebSocket, and SSE traffic, including proprietary combinations such as
WebSocket frames carrying a msgpack envelope with protobuf payloads. Users author Cases against
plain JSON-like data, so wire data must be decoded before matching and encoded after a Behavior.

Earlier notes framed this as "codec plugins", with separate future concepts for message transforms
and other stages. That risks a new config field, a new plugin kind, and an unclear order every time
a new need appears.

## Decision

- **One extension concept: the pipeline.** A Route (or config default) declares an ordered
  `pipeline` of steps. Inbound data runs the list in order; outbound data runs it reversed.
  Ordering is always explicit.
- **Built-in steps first** (amended by 0004: only `grpc-frame` and `protobuf`). `grpc-frame`, `protobuf`, `msgpack`, and `json` ship with Decoy.
  `protobuf` decodes dynamically from user-provided proto roots (descriptor sets as an extra input);
  no per-service plugin or code generation is required.
- **JMESPath for logic.** Case predicates and Behavior templating use JMESPath expressions over the
  decoded data, with Decoy standard functions.
- **Plugins export steps and functions, nothing else.** A plugin may provide named pipeline steps
  and named JMESPath functions. Built-in steps implement the same contract internally.
- **Plugins are long-running external processes.** Declared in config (manifest + executable, any
  language; amended by 0004: `command` only), started once with the runtime, called per message over stdio. Never one process per
  request. Plugins format data; network access is out of scope for now.
- **One Route shape for every transport.** `transport` + transport-specific `match`, Cases that
  match decoded data, Behaviors that produce transport-specific outcomes. Collections, Sessions, and
  Behavior addresses are unchanged.

## Consequences

- "Codec stage" and "message transform stage" collapse into **Pipeline step** in `CONTEXT.md`.
- Built-in steps must work before the plugin worker contract is frozen (amended by 0004: the worker
  contract comes first).
- WASM may later be a second loader for the same step/function contract.
- Custom matchers and custom Behavior kinds are not plugin extension points for now.
