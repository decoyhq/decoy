# Decoy

The normative dictionary for Decoy. When a term below is used in code, docs, or discussion, it
means _exactly_ this — one line per term, plus the synonyms to avoid. The prose explanation of how
the terms fit together lives in the [user guide](apps/docs/content/docs/index.md). Name = `decoy`; packages publish scoped as
`@decoyhq/*`, CLI bin `decoy`.

## Language

### Core concepts

**Collection**:
An ordered list of `route:case:behavior` activations — the unit switched to change the whole
**Scenario** atomically. Collections live in one collections YAML file and may inherit from another
Collection with `from`; child/local activations override parent activations by `route+case`.
_Avoid_: "preset" for this concept unless the product vocabulary is intentionally changed later,
"set", "group".

**Route**:
A coarse interaction matcher and namespace with a stable `id` and exactly one `transport`. For HTTP,
the Route match is the method + OpenAPI-style path such as `GET /users/{id}`. Route match answers
_where_ the interaction happens.
_Avoid_: "handler" (a Route serves no code), "endpoint" when it hides transport/case/behavior.

**Case**:
A named input-shape matcher inside a **Route**. Case match answers _which request/message shape_ is
active for that Route, such as HTTP `pathParams`, `query`, `headers`, or `body` constraints. Cases
own their Behaviors.
_Avoid_: "preset" (retired/provisional for this layer), "matcher" as the user-facing noun unless
precision is more important than beginner UX.

**Behavior**:
A named action/outcome inside a **Case**. A Behavior may be a static HTTP response, explicit
Passthrough, a gRPC reply, a WebSocket reply (`message`/`messages`, optional `close`), and later a
flow, file response, or replay behavior. Behaviors
have a canonical `kind`; simple HTTP responses may infer `kind: response`.
_Avoid_: "variant" (retired/provisional), "response" (a Behavior is addressable and may not be a
plain response), "mock".

**Scenario**:
Informal — the behavior a **Collection** encodes (`happy-path`, `checkout-fails`, `empty-state`).
_Avoid_: using interchangeably with **Collection** (the Collection is the artifact; the Scenario is
what it means).

**Behavior address**:
The `route:case:behavior` triple, e.g. `users-by-id:user-123:success` — how Collections and runtime
overrides name a Behavior. IDs in the triple must not contain `:`.
_Avoid_: "path" (collides with a Route's HTTP path), "key", "variant address".

### Axes

**Service axis**:
_Which upstream or transport address?_ Encoded by **Route** identity and route-level match.
_Avoid_: conflating with the **Scenario axis**.

**Scenario axis**:
_Which behavior now?_ Encoded in the active **Collection** plus per-route overrides.
_Avoid_: encoding scenario behavior in **Route** ids.

### Selection & sessions

**Selection**:
The mutable runtime choice — the active **Collection** plus per-`route+case` Behavior overrides.
Held per **Session**.
_Avoid_: "config" (Selection is runtime, not authored), broad "state" unless discussing flows later.

**Session**:
An isolated **Selection** scope keyed canonically by the `x-mock-session` header. The default/global
Session is for local dev; explicit Sessions isolate parallel e2e tests on a shared runtime.
`endSession` ends a Session: its Selection is dropped, and the Session starts again from defaults
the next time it is used. Past a fixed cap, the Session changed least recently is ended the same
way.
_Avoid_: "tenant", "context", "user".

**Catalog**:
The loaded, immutable authored model: every **Route** with its Cases and Behaviors, the
**Collections** (inheritance resolved once), and runtime config. Shared by all Sessions and
replaced whole on reload; a **Selection** chooses within it.
_Avoid_: "registry", "store", "config" (config is the file the Catalog is loaded from).

**SessionView**:
The runtime's per-request snapshot: the shared **Catalog** plus a copy of one **Session**'s
**Selection**, so a request resolves without holding locks across delays or passthrough. Internal
to `decoy-runtime`.
_Avoid_: confusing it with the **Router** (the public per-Session handle), "context".

### Matching & templating

**Request envelope**:
The fixed shape case matchers and templates evaluate against. For HTTP it includes method, URL,
path, `pathParams`, query, headers, cookies, and body. For a unary or server-streaming gRPC call
it is `service`, `method`, `metadata` (the request headers), and `message` (the decoded request, protobuf JSON by default).
An HTTP or gRPC Route with an input Pipeline matches Cases against its transformed body or message.
Other transports provide transport-specific message/connection data through the same contract.
_Avoid_: "context", "request object".

**Message envelope**:
The request envelope's counterpart for one inbound message on a long-lived connection (WebSocket,
SSE subscription, streaming gRPC): the decoded message plus the connection's path, query, headers,
and cookies. See the [WebSocket guide](apps/docs/content/docs/websocket/index.md#behaviors).
_Avoid_: "context", "frame" (a frame is the wire unit before the pipeline decodes it).

**Template context**:
The stable data available to response/message templates: request or message data, Session/Selection
identity, and bindings captured while matching the Route and Case.
_Avoid_: exposing arbitrary internals such as logs, route definitions, environment, or server state.

**Standard function**:
A built-in portable function available to matching/template expressions. Future extensions may add named functions.
_Avoid_: "helper", "custom JS function" for portable semantics.

### Invariants

**Fail-closed**:
A miss returns a diagnostic mock-miss response; it never reaches the real upstream unless
**Passthrough** is explicitly selected.
_Avoid_: "strict mode" (it is the default).

**Passthrough**:
An explicit Behavior that lets a matched request reach an upstream instead of being mocked.
Passthrough target resolution is behavior override → route override → global/runtime config →
request metadata/original host.
_Avoid_: "proxy" as Decoy's normal operation.

**Order-based matching**:
After Collection inheritance and overrides are resolved, active route entries are checked
bottom-to-top; the last matching selected Case wins. This supports fallback/override behavior.
Future validation should warn or error when broad later cases shadow specific earlier cases.
_Avoid_: pretending matching order is incidental.

### Control surfaces

**Controller**:
The control verbs for runtime Selection: `useCollection`, `useRoute`, `reset`, and `endSession`.
One-shot/temp behavior is roadmap.
_Avoid_: "admin", "manager".

**Control API**:
The cross-process mirror of the **Controller** plus catalog/read-model inspection, and `wait` (for a
live **Connection**) and `send` (to it). Resolution, recording, other Connection controls (close,
wait for a message) are roadmap/experimental until intentionally frozen.
_Avoid_: letting UI-only endpoints become the public adapter contract accidentally.

**Router**:
A first-class **Session** handle that exposes Controller verbs, `connection(route)` to wait for a
**Connection**, and session identity/header helpers.
Different integrations may implement it in-process, through a sidecar runtime, or over HTTP, but
must preserve the same semantic contract.
_Avoid_: "client" when referring to the per-Session handle.

**Connection**:
A **Session**'s live connections (WebSocket, streaming gRPC) on one **Route**, as returned by the
Router's `connection(route)` once one is live. `send` pushes a message to all of them, skipping
Cases. A reconnect is a new connection.
_Avoid_: "socket" for the Session-level handle (a socket is one client connection), "channel".

### Extension concepts

**Pipeline**:
An ordered series of **Pipeline steps** between transport payloads and the values Cases and Behaviors use. Named Pipelines have independent `input` and `output` lists, each in execution order. See ADR 0004.
_Avoid_: implicit step ordering.

**Transformer**:
A named definition for one reusable input or output transformation. A Transformer uses a persistent worker or commands for its supported directions.

**Pipeline step**:
One use of a Transformer in a Pipeline direction, or one of the built-in gRPC transforms.
_Avoid_: "codec stage", "middleware".

**Transformer worker**:
A persistent external process that implements the worker protocol for a **Transformer**, with or without the SDK.
_Avoid_: "hook", "extension" for this concept.
