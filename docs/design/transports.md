# Transports: worked examples

Status: **draft for review** (#216). Once agreed, this becomes the spec for gRPC (#217) and
WebSocket/SSE (#218). Nothing here is implemented except HTTP.

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
      <behavior>: <transport-specific outcome, templated, optional delay>
```

What doesn't change:

- **Collections** list `route:case:behavior`; **Sessions** isolate Selections; order is bottom-to-top.
- **Matching:** literal patterns plus `${ }` predicates, as for HTTP today.
- **Templating:** `${ }` in any outcome string; the built-in functions are the same.
- **Fail-closed:** a request or message nothing matches gets a transport-appropriate miss, never a
  real upstream.

What changes per transport is the `match` address, the **envelope** that Cases and templates see,
and the shape of a Behavior's outcome.

| transport | Route `match`       | unit a Case answers               | Behavior outcome                                 |
| --------- | ------------------- | --------------------------------- | ------------------------------------------------ |
| `http`    | `method`, `path`    | one request                       | `status`, `headers`, `body` / passthrough        |
| `grpc`    | `service`, `method` | one call (unary), or each message | `message` / `status` error; `stream` of messages |
| `ws`      | `path`              | each inbound message              | `send` list of messages, `close`                 |
| `sse`     | `path` (GET)        | the subscribing request           | `events` list                                    |

Server-first messages (WebSocket, streaming gRPC, SSE pushes) come from the test via the Control
API `send` (section 9), not from connect-time Cases.

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

pipelines: # named, reusable step lists (ADR 0002)
  approvals-ws:
    - msgpack # bytes ⇄ { type, data }
    - protobuf:
        field: data # decode envelope.data …
        typeFrom: type # … as the proto message named by envelope.type
```

Inbound data runs the steps top to bottom; outbound data runs them in reverse. A Route uses
`pipeline: approvals-ws` or an inline list. Defaults are per transport: gRPC Routes get
`[grpc-frame, protobuf]` (message types from `service`/`method`), HTTP keeps the JSON-by-content-type
body, WebSocket and SSE get none. A Route's explicit `pipeline` **replaces** the default entirely, in
the order written, e.g. `[msgpack, grpc-frame, protobuf]` for gRPC-framed protobuf inside msgpack
(**Q2**). Outside a gRPC Route, `protobuf` names its message type (`message:` or `typeFrom:`).

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
        # then the call ends with OK; add `status: ABORTED` after the list to end with an error
```

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
speaking first is a test-driven `send`.

```yaml
id: approvals-review
transport: grpc
match: { service: approvals.v1.Approvals, method: Review }
cases:
  comment:
    match: { message: { kind: COMMENT } }
    behaviors:
      ack:
        send: [{ message: { kind: ACK, ref: '${ message.id }' } }]
```

## 6. WebSocket

```yaml
id: notifications
transport: ws
match: { path: /ws/notifications }
cases: # each Case answers an inbound message
  subscribe:
    match: { message: { type: subscribe } }
    behaviors:
      updates:
        send:
          - { message: { type: subscribed, topic: '${ message.topic }' } }
          - { message: { type: update, topic: '${ message.topic }', count: 1 }, delay: 1s }
      reject:
        send: [{ message: { type: error, reason: forbidden } }]
        close: { code: 4003, reason: forbidden }
  ping:
    match: '${ message == `"ping"` }' # text frames that aren't JSON stay strings
    behaviors:
      pong: { send: [{ message: pong }] }
```

- **Message envelope:** `{ path, pathParams, query, headers, cookies }` of the upgrade request plus
  `message`, which is the decoded inbound message: JSON when it parses, the text otherwise, or the
  pipeline's output.
- **Outbound:** `send` items go out in order, each after its own `delay`. A `message` that is an
  object or array is sent as JSON text unless a pipeline encodes it. `close` closes the connection
  afterwards.
- **Unmatched message:** skipped. Decoy sends nothing, logs a miss, and keeps the connection open
  (**Q4**).
- **Connecting:** an upgrade to a `ws` Route's path is always accepted, and the server says nothing
  until a message arrives or the test calls `send` (section 9). An upgrade to a path no `ws` Route
  matches is rejected with the HTTP miss.

## 7. WebSocket + msgpack envelope + protobuf payload

Frames are msgpack maps `{ type: "approvals.v1.Approval", data: <protobuf bytes> }`. With the
`approvals-ws` pipeline from section 2, Cases and Behaviors only ever see JSON:

```yaml
id: approvals-live
transport: ws
match: { path: /ws/approvals }
pipeline: approvals-ws
cases:
  watch:
    match:
      message: { type: approvals.v1.WatchRequest, data: { ids: [a-1] } }
    behaviors:
      approved:
        send:
          - message:
              type: approvals.v1.Approval
              data: { id: a-1, state: APPROVED }
            delay: 200ms
```

Inbound: msgpack decodes the frame to `{ type, data: <bytes> }`, then protobuf decodes `data` as
`WatchRequest` (named by `type`). Outbound runs in reverse: protobuf encodes `data` as `Approval`,
then msgpack encodes the map. If the work format differs (an extra header, compression,
encryption), a plugin step slots into the same list (ADR 0002).

## 8. Server-Sent Events

```yaml
id: approval-events
transport: sse
match: { path: /api/approvals/{id}/events }
cases:
  any:
    behaviors:
      progress:
        events:
          - { event: status, data: { id: '${ pathParams.id }', state: PENDING } }
          - { event: status, data: { state: APPROVED }, id: '2', delay: 1s }
        # stream stays open afterwards; `end: true` closes it
      keep-quiet:
        events: [] # open stream; the test pushes events with `send`
```

- **Envelope:** the request fields `{ path, pathParams, query, headers, cookies }`, so Cases can
  match the subscription like any GET.
- **`data`:** an object or array is serialized as JSON, a string is sent as-is. `event`, `id` and
  `retry` map to the SSE fields.
- **Unmatched GET:** the HTTP miss (501).

## 9. Pushing from a test: Control API `send`

Long-lived connections need test-driven messages, for example "the client's logic depends on the
server's first message", or "trigger a UI update now". This is the only way the server speaks first.

```http
POST /__decoy__/control/send
x-mock-session: test-42
content-type: application/json

{ "route": "notifications", "message": { "type": "update", "count": 5 } }
```

- **Who receives it:** every live connection of that Route **in that Session**, and nobody else.
  The message goes through the Route's pipeline. For SSE, `message` is an `{ event, data, id }`
  item.
- **Response:** `200 { "delivered": <n> }`. `404` for an unknown Route, `409` for a Route that isn't
  ws/sse/streaming gRPC.
- **No live connection:** the call waits for one up to `timeout` (milliseconds, default 5000), then
  delivers. On timeout it fails with `408`: something is wrong with the test or app (**Q5**).

### Playwright

```ts
test('shows the approval as soon as the server pushes it', async ({ page, decoy }) => {
  await page.goto('/approvals/a-1')
  const live = await decoy.connection('approvals-live') // waits for the page's socket in this Session
  await live.send({ type: 'approvals.v1.Approval', data: { id: 'a-1', state: 'APPROVED' } })
  await expect(page.getByRole('status')).toHaveText('Approved')
})
```

- `decoy.connection(routeId, { timeout })` resolves once this test's Session has a live connection
  on the Route. `.send(message)` calls the Control API.
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
  to its **next** inbound message or event, not retroactively.

## 11. Open questions

| #   | question                                                          | recommendation                                                                                                                                          |
| --- | ----------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Q1  | One port for HTTP/WS/SSE/gRPC, or a separate `grpcPort`?          | **Decided:** one port by default (per-request dispatch on `content-type`); extra listeners via config, e.g. `listeners: { grpc: 50051 }` (was `ports`). |
| Q2  | Implicit `[grpc-frame, protobuf]` pipeline for gRPC Routes?       | **Decided:** per-transport defaults (gRPC → `[grpc-frame, protobuf]`); an explicit `pipeline` replaces the default entirely, in written order.          |
| Q3  | Validate gRPC message literals against the proto at startup?      | **Decided:** yes, wherever a `protobuf` step runs: response literals and literal `match` fields; `${ }` is checked at render time.                      |
| Q4  | Unmatched WS message: silent + recorded, or close the socket?     | **Decided:** skip it: send nothing, log a miss, keep the connection open.                                                                               |
| Q5  | `send` with no live connection: wait or fail fast?                | **Decided:** wait for a connection up to `timeout` (default 5 s); fail with `408` on timeout.                                                           |
| Q6  | WebSocket routing in Playwright: bridge via `routeWebSocket`?     | **Decided:** yes; the fixture adds the Session header, same as `context.route` for HTTP.                                                                |
| Q7  | SSE in Playwright needs app and Decoy on the same protocol. OK?   | **Decided:** TLS listeners with a provided cert or a generated self-signed one (#241); also needed for gRPC over TLS.                                   |
| Q8  | `__decoy_session` query fallback for browsers outside Playwright? | **Deferred** until non-Playwright runners (Cypress, Selenium) are in scope.                                                                             |
| Q9  | Glossary: "Request envelope" / "Message envelope".                | **Decided:** adopt both in `CONTEXT.md`; rename `RequestContext` → `RequestEnvelope` (#240).                                                            |
