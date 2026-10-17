# Decoy next direction

Status: working design note, not user documentation.

The old `apps/docs` documentation site has been removed while the product model is being reset. New
user docs should be recreated from this design note, `CONTEXT.md`, and ADRs once the native runtime
prototype has a stable shape.

## Product shape

Decoy is a general-purpose local mock tool. JS/TS remains important because the current ecosystem is
frontend-heavy, but the product center is a portable runtime, simple CLI, and stable semantics that
adapters can share.

## Runtime direction

The next serious implementation direction is a Rust native runtime/binary. The current TS
implementation is evidence, not law: preserve intentionally chosen semantics, not accidental current
behavior.

First milestone priority is HTTP runtime. Plugin and WebSocket design must be kept in mind, but not
implemented before the runtime is useful.

## Semantic model

Canonical vocabulary:

```txt
collection -> route -> case -> behavior
```

- Collection: ordered list of `route:case:behavior` activations, with `from` inheritance.
- Route: one file, explicit `id`, exactly one top-level `transport`.
- Route match: where/address, e.g. HTTP `method` + `/users/{id}`.
- Case: named input matcher inside a route.
- Behavior: named action/outcome inside a case; has canonical `kind`, with simple HTTP response kind inferred.

Collections stay simple. They select behavior addresses; behavior details live in route files.

## Matching and selection

Resolved collection route order is semantic. Active entries are checked bottom-to-top; the last
matching selected case wins. This enables fallback/override behavior.

Prototype may do this silently. Future validation should detect shadowing/ambiguity where feasible
and explain runtime matching decisions clearly.

## HTTP milestone scope

Ship first:

- YAML/JSON route files and collections file.
- HTTP `transport`.
- Route/case/behavior loading and validation.
- Collections with `id`, `from`, `routes`.
- `routes` entries as `route:case:behavior`; structured form optional.
- No implicit defaults for case/behavior.
- Response and passthrough behavior kinds.
- Fail-closed miss with descriptive diagnostic response.
- Sessions via `x-mock-session`.
- Control: `useCollection`, `useRoute`, `reset`.
- Golden tests for matching, response plan, collection inheritance/order, passthrough plan, and session isolation.

## Milestone order

Agreed 2026-10-17. The HTTP milestone above is done (#154).

1. Repo cleanup: legacy TS packages and examples removed; TS lives on in git history as prior art.
2. Playwright E2E fixture (#178): native-runtime-backed fixture plus `examples/playwright-e2e`.
3. Alpha release: cross-compiled binaries on GitHub Releases, npm wrapper (esbuild/biome pattern),
   Rspress docs site with generated `llms.txt`. Until then, guide pages are Markdown in
   `docs/guide/`.
4. Matching parity: query/headers/body Case matching, JMESPath predicates, templating, delay.
5. Transports design note: worked examples for HTTP, gRPC unary/streaming, WebSocket, SSE,
   WebSocket + msgpack + protobuf, and test-driven `send`. Reviewed before gRPC code.
6. gRPC unary, then streaming.
7. WebSocket and SSE: `onConnect`, Behaviors as lists of outgoing messages with delays, Control API
   `send` to a Route's live connections in a Session, Playwright `decoy.connection(routeId)`.
8. Plugin worker (ADR 0002).

Deferred: client message/request log for assertions, flows/sequences, recording, UI, custom
matchers or Behavior kinds, plugin network access, WASM plugin loader.

## Languages and wrappers

The `decoy` binary is the product. Wrappers in other languages (Node Playwright first, then Python
and Go Playwright) are thin clients: start or reuse the binary, stamp the Session header, call the
HTTP Control API. No FFI bindings. The Control API must be frozen as a public contract before the
second-language wrapper.

## Extension model

See ADR 0002: one ordered `pipeline` of steps (built-in `grpc-frame`, `protobuf` from proto roots,
`msgpack`, `json`, or plugin steps), JMESPath for matching and templating, and plugins as
long-running external processes that export steps and functions.
