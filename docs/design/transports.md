# Transports: worked examples

Status: **draft for review** (#216). Once agreed, this becomes the spec for gRPC (#217) and
WebSocket/SSE (#218). Implemented: HTTP, gRPC unary and server streaming, and WebSocket text frames
without pipelines (section 6; `every`, Control API `close` and the `wait` conditions other than
`connected: true` not yet).

Read it as "what would I write in my mocks". Every open choice is marked **Q** with a recommendation
at the end.

## 1. One shape for every transport

```yaml
id: <route id>
transport: http | grpc | ws | sse
match: <where: transport-specific address>
pipeline: <optional: how bytes become JSON (ADR 0002)>
cases:
  <case>:
    match: <literal fields and/or ${ } predicates over the envelope, as for HTTP today>
    behaviors:
      <behavior>: <outbound: `message` or `messages`, plus transport extras; templated, optional delay>
```

What doesn't change:

- **Collections** list `route:case:behavior`; **Sessions** isolate Selections; order is bottom-to-top.
- **Matching:** literal patterns plus `${ }` predicates, as for HTTP today.
- **Templating:** `${ }` in any outcome string; the built-in functions are the same.
- **Fail-closed:** a request or message nothing matches gets a transport-appropriate miss, never a
  real upstream.

What changes per transport is the `match` address, the **envelope** that Cases and templates see,
and the extras next to the outbound payload.

Every transport works the same way: something comes in, a Case matches it, its Behavior answers.
Separately, a test can push to a live connection at any moment (Control API `send`, section 9).

```
inbound (request / message / connection opened) → Case → Behavior → outbound
test → Control API send                                            → outbound (no Case)
```

WebSocket and SSE use the following fields for outgoing payloads:

- `message`: one payload.
- `messages`: a list of `{ payload, delay? }` items, sent in order.
- A Behavior-level `delay` starts the whole Behavior after that wait. An item delay adds a wait before that item. The first item waits for both delays when both are present. See the [WebSocket Routes PRD](websocket-routes-prd.md).

| transport | Route `match`       | unit a Case answers                               | one reply | several replies | extras                                 |
| --------- | ------------------- | ------------------------------------------------- | --------- | --------------- | -------------------------------------- |
| `http`    | `method`, `path`    | one request                                       | `body`    | —               | `status`, `headers` / passthrough      |
| `grpc`    | `service`, `method` | one call (unary), or each message                 | `message` | `stream`        | `status`, `metadata`                   |
| `ws`      | `path`, `query`     | each inbound message, or the open (`on: connect`) | `message` | `messages`      | `close`                                |
| `sse`     | `path` (GET)        | the subscribing request                           | —         | `messages`      | per item `event`, `id`, `retry`; `end` |

The examples use `messages` for WebSocket and SSE. gRPC server streaming uses `stream`.

## 2. Config: protos and pipelines

```yaml
# decoy.yaml
routes: routes
collections: collections.yaml
port: 8080 # HTTP/1.1, WebSocket, SSE, and gRPC (HTTP/2 cleartext) on the same port   (Q1)
# listeners: { grpc: 50051 }   # optional extra listeners; same Routes and Sessions

protos:
  roots: [../proto] # import paths; every .proto under them is compiled at startup
  # descriptorSets: [build/api.binpb]   # alternative for hard-to-compile setups

transformers:
  approvals:
    worker: [node, ./plugins/approvals.ts]
  gzip:
    input: [gzip, -dc]
    output: [gzip, -c]

pipelines:
  approvals-ws:
    input: [gzip, approvals]
    output: [approvals, gzip]
```

A Route selects a named Pipeline with `pipeline: approvals-ws`. Each direction runs in the order
written. A Transformer reference selects the worker callback or command for that direction. A
direction with no steps uses its transport default.

The gRPC default remains `[grpc-frame, protobuf]`, typed by `service` and `method`. HTTP still uses
its usual body handling. WebSocket without an input Pipeline parses JSON text when possible, keeps
other text as a string, and records a miss for binary frames. See the [Transport transformers PRD](transport-transformers-prd.md).

On a single port, one entry handler dispatches per request: `/__decoy__/*` → Control API,
`content-type: application/grpc*` → gRPC, everything else → HTTP (so REST over HTTP/2 works).

## 3. HTTP (today, for comparison)

```yaml
id: users-by-id
transport: http
match: { method: GET, path: /api/users/{id} }
cases:
  user-1:
    match: { pathParams: { id: '1' } }
    behaviors:
      ada: { body: { id: '1', name: Ada Lovelace } }
```

Envelope: `{ method, url, path, pathParams, query, headers, cookies, body }`.

## 4. gRPC unary

`approvals.proto` (excerpt):

```proto
package approvals.v1;
service Approvals {
  rpc GetApproval(GetApprovalRequest) returns (Approval);
  rpc WatchApprovals(WatchRequest) returns (stream Approval);
  rpc UploadEvidence(stream Evidence) returns (UploadResult);
  rpc Review(stream ReviewEvent) returns (stream ReviewEvent);
}
message GetApprovalRequest { string id = 1; }
message Approval { string id = 1; State state = 2; repeated string approvers = 3; }
enum State { STATE_UNSPECIFIED = 0; PENDING = 1; APPROVED = 2; REJECTED = 3; }
```

```yaml
id: approvals-get
transport: grpc
match:
  service: approvals.v1.Approvals # fully qualified; must exist in the protos (startup error otherwise)
  method: GetApproval
cases:
  pending:
    match:
      message: { id: a-1 } # the decoded request, as protobuf JSON (camelCase, enums as names)
    behaviors:
      ok:
        message: { id: a-1, state: PENDING, approvers: [alice] }
      approved:
        message: { id: a-1, state: APPROVED, approvers: [alice, bob] }
  any:
    behaviors:
      echo:
        message: { id: '${ message.id }', state: PENDING }
      not-found:
        status: NOT_FOUND # any gRPC status name
        message: 'approval ${ message.id } not found' # status message, templated
        metadata: { x-reason: missing } # trailers
      slow:
        delay: 3s
        message: { id: '${ message.id }', state: PENDING }
```

- **Envelope:** `{ service, method, metadata, message }`. `metadata` is the request headers,
  lowercased; `message` is the decoded request.
- **Behavior:** `message` (a success response), or `status` plus an optional `message` string (an
  error). `metadata` sets headers/trailers.
- **Validation at startup:** the message must fit the proto. Unknown fields or a wrong enum name
  fail with `route:case:behavior` and the field path (**Q3**).
- **Miss:** gRPC `UNIMPLEMENTED` with the same diagnostic text as an HTTP miss, and an
  `x-mock-miss: true` trailer.
- **Session:** the `x-mock-session` metadata key; that's a normal gRPC header, so test clients just
  set it.

`collections.yaml` is unchanged:

```yaml
- id: happy-path
  routes:
    - approvals-get:any:echo
    - approvals-get:pending:ok
- id: approvals-down
  from: happy-path
  routes:
    - approvals-get:any:not-found
```

## 5. gRPC streaming

**Server streaming:** one request, a timed list of responses.

```yaml
id: approvals-watch
transport: grpc
match: { service: approvals.v1.Approvals, method: WatchApprovals }
cases:
  any:
    behaviors:
      progress:
        stream:
          - { message: { id: a-1, state: PENDING } }
          - { message: { id: a-1, state: APPROVED }, delay: 500ms }
        status: OK # ends the call; `status: ABORTED` ends it with an error
```

Today this list is `stream:`; `messages:` is the name after the Q11 rename.

With `status`, the call ends after the list. Without it, the call stays open after the list and
each Control API `send` (§9) to the Route from the call's Session is another response message,
until the client cancels.

**Client streaming:** the Case sees all request messages once the client finishes sending.

```yaml
id: approvals-upload
transport: grpc
match: { service: approvals.v1.Approvals, method: UploadEvidence }
cases:
  any:
    match: '${ length(messages) > `0` }' # envelope has `messages` (list) instead of `message`
    behaviors:
      ok:
        message: { received: '${ length(messages) }' }
```

**Bidirectional:** works like WebSocket (section 6). Cases answer each inbound message; the server
speaking first is a test-driven `send` (`on: connect` is WebSocket-only for now).

```yaml
id: approvals-review
transport: grpc
match: { service: approvals.v1.Approvals, method: Review }
cases:
  comment:
    match: { message: { kind: COMMENT } }
    behaviors:
      ack:
        message: { kind: ACK, ref: '${ message.id }' }
```

## 6. WebSocket

Two services talk to the page over sockets on the same path, told apart by `service`:

```
wss://my-website.com/subscribe?client=browser&service=my-friend-webservice
wss://my-website.com/subscribe?client=browser&service=my-best-friend-service
```

The page starts using a socket once the server sends `ping`. After that it sends requests with an
`id` and expects a reply carrying the same `id`.

One Route per socket:

```yaml
# routes/friend-socket.yaml
id: friend-socket
transport: ws
match:
  path: /subscribe
  query: { service: my-friend-webservice } # other params (client=browser) are ignored
cases:
  hello:
    on: connect # runs once, when the socket opens
    behaviors:
      ping: { message: ping }
      silent: { messages: [] } # stay quiet; the test sends ping (section 9)
      unauthorized:
        message: { type: error, reason: unauthorized }
        close: { code: 4001, reason: unauthorized }
  status:
    match: { message: { type: get-status } } # runs on each matching message from the page
    behaviors:
      online: { message: { id: '${ message.id }', status: online } }
      offline: { message: { id: '${ message.id }', status: offline } }
  subscribe:
    match: { message: { type: subscribe } }
    behaviors:
      updates:
        messages:
          - { payload: { id: '${ message.id }', type: subscribed, topic: '${ message.topic }' } }
          - { payload: { type: update, topic: '${ message.topic }', count: 1 }, delay: 1s }
```

```yaml
# routes/best-friend-socket.yaml
id: best-friend-socket
transport: ws
match:
  path: /subscribe
  query: { service: my-best-friend-service }
cases:
  hello:
    on: connect
    behaviors:
      ping: { message: ping }
      silent: { messages: [] }
  # ...its own message Cases
```

```yaml
# collections.yaml
- id: happy-path # both sockets start on their own
  routes:
    - friend-socket:hello:ping
    - friend-socket:status:online
    - friend-socket:subscribe:updates
    - best-friend-socket:hello:ping

- id: manual-start # sockets open and wait; the test starts them
  from: happy-path
  routes:
    - friend-socket:hello:silent
    - best-friend-socket:hello:silent
```

With `decoy serve` and no test, the page works on its own:

```
page opens /subscribe?…service=my-friend-webservice   → Route friend-socket      → hello:ping   → "ping"
page opens /subscribe?…service=my-best-friend-service → Route best-friend-socket → hello:ping   → "ping"
page sends { id: 7, type: get-status }               → friend-socket status:online → { id: 7, status: online }
page sends { type: nope }                            → no Case: nothing sent, miss logged, socket stays open
page opens /subscribe?service=unknown                → no ws Route: handled as an HTTP GET (501 miss)
```

Tests change which Behaviors run, or push messages themselves (section 9):

```ts
test('works when both services are up', async ({ page }) => {
  await page.goto('/') // happy-path: both sockets get ping when they open
  await expect(page.getByText('Friend: online')).toBeVisible()
})

test('shows connecting until the friend service says ping', async ({ page, decoy }) => {
  await decoy.useCollection('manual-start')
  await page.goto('/')
  await expect(page.getByText('Friend: connecting…')).toBeVisible()

  const friend = await decoy.connection('friend-socket')
  await friend.send('ping') // only the friend socket gets it
  await expect(page.getByText('Friend: online')).toBeVisible()
  await expect(page.getByText('Best friend: connecting…')).toBeVisible()
})

test('friend goes offline', async ({ page, decoy }) => {
  await decoy.useRoute('friend-socket', 'status', 'offline')
  await page.goto('/')
  await expect(page.getByText('Friend: offline')).toBeVisible()
})
```

- **Route `match`:** `path`, plus optional `query` with the same rules as a Case `query` (listed
  keys must be present, others are ignored). Several `ws` Routes can share a path. When more than
  one matches an upgrade, the one with more `query` keys wins; an equal tie is a config error at
  load (**Q12**).
- **Message envelope:** `{ path, pathParams, query, headers, cookies }` of the upgrade request plus
  `message`, the decoded inbound message: JSON when a text frame parses, the text otherwise, or the
  pipeline's output (section 2). Text frames that aren't JSON stay strings:
  ``match: '${ message == `"ping"` }'``. A binary frame with no pipeline is a MISS.
- **`on: connect` Case:** runs once when the upgrade is accepted, before any inbound message. It
  has no `match` and its envelope has no `message`; templates still see the upgrade request
  (`${ query.service }`). At most one per Route. Its Behavior is chosen like any other, so
  Collections and `useRoute` turn it on, off or into a rejection (**Q10**).
- **Outbound:** `message` or `messages`. Without a pipeline, an object or array is sent as JSON text
  and a string as text. With one, the last step decides the frame kind: it returns a string (text
  frame) or bytes (binary frame), so one plugin can speak JSON control text next to binary data.
- **Scheduling:** within one Behavior, items go out in order, each `delay` after the one before it
  (section 1). Behaviors don't queue behind each other: if the page sends B while A's Behavior still
  has a delayed item, B's replies go out on their own timers and interleave with A's (**Q16**).
- **Repeating:** an item with `every: 30s` repeats until the connection closes; the first send is
  one interval in (add `delay` to shift it). Follow-up slice, not the core one (**Q17**).
- **Closing:** the connection normally stays open for the whole test. `close: { code, reason }`
  closes it after the Behavior's messages, to test reconnect or error handling. Closing cancels the
  connection's pending delayed and repeating items. In an `on: connect` Behavior the socket opens
  and then closes; refusing the upgrade with an HTTP status isn't supported for now. When the client
  closes, its pending items are dropped and it leaves the `send` registry (**Q18**).
- **Keep-alive:** protocol-level ping/pong control frames are answered automatically and never reach
  Cases. An app-level `"ping"` message is an ordinary message.
- **Unmatched message:** skipped. Decoy sends nothing, logs a miss, and keeps the connection open
  (**Q4**).
- **Connecting:** an upgrade matching a `ws` Route is always accepted. Without an `on: connect`
  Case (or with one whose Behavior sends nothing), the server says nothing until a message arrives
  or the test calls `send`. An upgrade no `ws` Route matches (or one a repeated query key ties) is handled as an HTTP GET:
  an HTTP Route on that path answers it, otherwise the HTTP miss (501).

## 7. WebSocket Transformers

A WebSocket Route can transform application payloads before matching or after rendering. In the
approvals example, `gzip` handles the outer compression and the approvals worker handles the
MessagePack envelope and the protobuf schema selected by its topic. The worker uses Decoy's loaded
protobuf schemas through the SDK. Cases and Behaviors use the application fields.

Workers are persistent processes. `definePlugin` takes `input` and `output` callbacks. A worker can
keep per-connection state and use WebSocket frame metadata. See the [worker protocol](plugin-protocol.md)
for the language-neutral handshake, framing, data types, and host calls. Worker calls can return a
miss on input or report a transform failure. A worker process exiting stops Decoy.

One-shot commands run with the configured argv in the directory of `decoy.yaml`. The command reads
stdin to EOF, writes its result to stdout, and uses stderr for logs. A nonzero exit fails the
transform. A terminal WebSocket input command emits JSON for the Message envelope. The last output
worker selects text or binary by its return value. A terminal output command sends a binary frame.

## 8. Server-Sent Events

```yaml
id: approval-events
transport: sse
match:
  path: /api/approvals/{id}/events
cases:
  any:
    behaviors:
      progress:
        messages:
          - { event: status, payload: { id: '${ pathParams.id }', state: PENDING } }
          - { event: status, payload: { state: APPROVED }, id: '2', delay: 1s }
        # stream stays open afterwards; `end: true` closes it
      keep-quiet:
        messages: [] # open stream; the test pushes events with `send`
```

- **Envelope:** the request fields `{ path, pathParams, query, headers, cookies }`, so Cases can
  match the subscription like any GET.
- **`payload`:** the SSE `data` field. An optional `output` Pipeline transforms it before text framing. `event`, `id` and `retry` map to the SSE fields of the same name.
- **Unmatched GET:** the HTTP miss (501).

## 9. Pushing from a test: Control API `send`

Long-lived connections need test-driven messages, for example "the client waits for the server's
first message and the test decides when it comes", or "trigger a UI update now". The other way
the server speaks first is an `on: connect` Case (section 6), which also works without a test.

```http
POST /__decoy__/control/send
x-mock-session: test-42
content-type: application/json

{ "route": "notifications", "message": { "type": "update", "count": 5 } }
```

- **Who receives it:** every live connection of that Route **in that Session**, and nobody else.
  It skips Cases: the message goes straight out, through the Route's pipeline. For SSE, `message`
  is the `data` and the body may add `event`, `id` and `retry`, as in a `messages` item.
- **Response:** `200 { "delivered": <n> }`, the connections that queued the message. `404` for an unknown Route, `409` for a Route that isn't
  ws/sse/streaming gRPC.
- **No live connection:** the call waits for one up to `timeout` (milliseconds, default 5000,
  capped at 5 minutes), then delivers. On timeout it fails with `408`: something is wrong with the
  test or app (**Q5**).
- **Slow reader:** each connection queues up to 64 unread messages. One whose queue is full skips
  the message and doesn't count in `delivered`; with every one full, `send` waits as above.
- **Transform failure:** the output Pipeline runs once per recipient when the message is sent. If
  it fails, Decoy reports the error and skips the frame. `delivered` counts the connections that
  queued the message.

The other Control API verbs on live connections are scoped the same way (Route + Session):

- **`POST /__decoy__/control/close`** `{ route, code?, reason? }` (follow-up slice) closes the
  Session's connections on the Route, to test reconnect or error UI. `200 { "closed": <n> }`
  (**Q18**).
- **`POST /__decoy__/control/wait`** waits inside Decoy and returns when the condition holds, or
  `408` after `timeout` (default 5000) (**Q19**):
  - `{ route, connected: true }` (built): until the Session has a live connection on the Route that
    `send` would reach now (opened with the current output type); returns `{ "connections": <n> }`.
    `404`/`409` as for `send`. WebSocket sockets always count; a gRPC stream whose Case ends with a
    `status` is never registered, so `wait` answers `408` for it.
  - `{ route, message: <partial match> }` (follow-up slice): until the client has sent a matching
    message; returns the decoded message. Matching uses Case `match` rules over what Decoy recorded
    for that Session.
  - `{ route, settled: true }` (follow-up slice): until no delayed or scheduled item is left for
    that Route.

### Playwright

```ts
test('shows the update as soon as the server pushes it', async ({ page, decoy }) => {
  await page.goto('/')
  const friend = await decoy.connection('friend-socket') // waits for the page's socket in this Session
  await friend.send({ type: 'update', topic: 'news', count: 5 })
  await expect(page.getByText('news: 5')).toBeVisible()
})
```

- `decoy.connection(route, { timeout })` resolves once this test's Session has a live connection
  on the Route. `.send(message)` and `.close({ code, reason })` call the Control API.
- **Reconnects:** a client's new socket is a new connection. Its `on: connect` Case runs with the
  Selection active at that moment, and a new `decoy.connection(route)` resolves to it (**Q20**).

```ts
test('page reconnects after the server drops it', async ({ page, decoy }) => {
  await page.goto('/')
  const friend = await decoy.connection('friend-socket')
  await decoy.useRoute('friend-socket', 'status', 'offline') // applies to the new socket
  await friend.close({ code: 1011, reason: 'restart' })
  await decoy.connection('friend-socket') // the page's new socket
  await expect(page.getByText('Friend: offline')).toBeVisible()
})
```

- **Routing WebSockets** (Playwright ≥ 1.48 `context.routeWebSocket`): the fixture bridges each
  matching page socket to Decoy over its own WebSocket and adds `x-mock-session` on that upgrade.
  The page needs no changes. Config: `decoyOptions.webSocketUrls`, alongside `urls` (**Q6**).
- **Routing SSE:** `route.fulfill` buffers whole bodies, so streams can't go through it. SSE
  requests use `route.continue({ url: <decoy url>, headers })` instead, which streams. That needs
  the same protocol on both sides: http app → http Decoy, https app → https Decoy (TLS, #241) (**Q7**).

## 10. Sessions on long-lived connections

- **Non-browser clients** (backend services, gRPC tests): send `x-mock-session` on the upgrade or
  call, as with HTTP.
- **Browsers** can't set headers on `new WebSocket()` or `EventSource`. Under Playwright the fixture
  bridge adds the header, exactly as `context.route` does for HTTP. Other runners (Cypress, Selenium)
  are deferred; a `__decoy_session` query fallback is the candidate when they come (**Q8**).
- A connection keeps the Session it opened with. Control calls (`useCollection`, `useRoute`) apply
  to its **next** inbound message or event, not retroactively. An `on: connect` Case already ran, so
  a change to it affects the Session's next connection.

Two tests run in parallel against one Decoy and the same `friend-socket` Route (section 6). Each
test has its own Session, so the second test's override never reaches the first:

```ts
// worker 1, Session s-1
test('friend is online', async ({ page }) => {
  await page.goto('/') // socket opens with x-mock-session: s-1
  await expect(page.getByText('Friend: online')).toBeVisible()
})

// worker 2, Session s-2, at the same time
test('friend is offline', async ({ page, decoy }) => {
  await decoy.useRoute('friend-socket', 'status', 'offline') // only Session s-2
  await page.goto('/') // socket opens with x-mock-session: s-2
  await expect(page.getByText('Friend: offline')).toBeVisible()
})
```

```
s-1 socket: open → hello:ping → "ping" · { id: 1, type: get-status } → status:online  → online
s-2 socket: open → hello:ping → "ping" · { id: 1, type: get-status } → status:offline → offline
```

`send` is scoped the same way: `(await decoy.connection('friend-socket')).send(...)` in worker 2 reaches only
the s-2 socket.

## 11. Open questions

| #   | question                                                          | recommendation                                                                                                                                                                                   |
| --- | ----------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Q1  | One port for HTTP/WS/SSE/gRPC, or a separate `grpcPort`?          | **Decided:** one port by default (per-request dispatch on `content-type`); extra listeners via config, e.g. `listeners: { grpc: 50051 }` (was `ports`).                                          |
| Q2  | Implicit `[grpc-frame, protobuf]` pipeline for gRPC Routes?       | **Decided:** per-transport defaults (gRPC → `[grpc-frame, protobuf]`); an explicit `pipeline` replaces the default entirely, in written order.                                                   |
| Q3  | Validate gRPC message literals against the proto at startup?      | **Decided:** yes, wherever a `protobuf` step runs: response literals and literal `match` fields; `${ }` is checked at render time.                                                               |
| Q4  | Unmatched WS message: silent + recorded, or close the socket?     | **Decided:** skip it: send nothing, log a miss, keep the connection open.                                                                                                                        |
| Q5  | `send` with no live connection: wait or fail fast?                | **Decided:** wait for a connection up to `timeout` (default 5 s); fail with `408` on timeout.                                                                                                    |
| Q6  | WebSocket routing in Playwright: bridge via `routeWebSocket`?     | **Decided:** yes; the fixture adds the Session header, same as `context.route` for HTTP.                                                                                                         |
| Q7  | SSE in Playwright needs app and Decoy on the same protocol. OK?   | **Decided:** TLS listeners with a provided cert or a generated self-signed one (#241); also needed for gRPC over TLS.                                                                            |
| Q8  | `__decoy_session` query fallback for browsers outside Playwright? | **Deferred** until non-Playwright runners (Cypress, Selenium) are in scope.                                                                                                                      |
| Q9  | Glossary: "Request envelope" / "Message envelope".                | **Decided:** adopt both in `CONTEXT.md`; rename `RequestContext` → `RequestEnvelope` (#240).                                                                                                     |
| Q10 | Server-first WS messages without a test?                          | **Decided:** an `on: connect` Case: no `match`, at most one per Route, Behavior chosen by Selection. WebSocket only for now.                                                                     |
| Q11 | One name for outbound payloads across transports?                 | **Decided:** WebSocket and SSE use `messages` items with `payload` and optional `delay`. gRPC keeps its existing `stream`. |
| Q12 | Two WS sockets on one path, told apart by query?                  | **Decided:** `query` in a `ws` Route's `match`; more `query` keys wins, an equal tie fails at load.                                                                                              |
| Q13 | Which pipeline steps ship with Decoy?                             | **Decided:** gRPC defaults to `grpc-frame` and `protobuf`. Application Transformers use workers or commands. |
| Q14 | Frames that carry no type marker at all?                          | **Decided:** the plugin's job; Decoy never guesses a proto type.                                                                                                                                 |
| Q15 | Plain JSON and binary on one socket?                              | **Decided:** the plugin sees `ctx.frame` inbound and picks the kind outbound (string → text, bytes → binary). No `frame` key in config or `send`.                                                |
| Q16 | A message arrives while another Behavior has delayed items?       | **Decided:** interleave; each Behavior's items run on their own timers, in order, each `delay` after the previous one.                                                                           |
| Q17 | Repeating messages (heartbeat)?                                   | **Decided:** `every` on a `messages` item, until close. Follow-up slice.                                                                                                                         |
| Q18 | Closing a socket from config or from a test?                      | **Decided:** Behavior `close` after its messages; Control API `close` verb. Both cancel pending items. No HTTP-status rejection of upgrades for now.                                             |
| Q19 | Waiting on what the client sent, or until Decoy is done?          | **Deferred:** `waitForMessage` / `settled` have open questions (consume rules, history bound, `every`). Not in the first slices.                                                                 |
| Q20 | A client reconnects: same or new connection?                      | **Decided:** new connection; `on: connect` runs again with the current Selection.                                                                                                                |
| Q21 | Plugin per-call timeout?                                          | **Decided:** 5s default, `timeout:` per plugin. A timeout is a plugin error, not a MISS.                                                                                                         |
| Q22 | The plugin process exits?                                         | **Decided:** Decoy logs exit code and stderr tail, then exits non-zero. No restart for now; `restart:` could be added later without breaking anything.                                           |
| Q23 | Encode or plugin failure on a live connection?                    | **Decided:** report the transform error and skip the frame. The socket stays open. Control API `send` counts queue acceptance. |
