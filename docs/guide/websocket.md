# WebSocket

Decoy accepts WebSocket upgrades on its normal port, next to plain HTTP, gRPC and the Control API.
Point the client's socket URL at Decoy. With [TLS](tls.md) on, the same port serves `wss://`.

Supported so far: text frames carrying JSON or plain text, `on: connect` Cases, timed replies,
`close`, Control API `send`, and binary formats through [plugin steps](#binary-formats-plugin-steps).

## WebSocket Routes

One Route per socket. `match` takes the upgrade `path` and, optionally, `query` params the upgrade
must carry, so several sockets can share one path:

```yaml
# mocks/routes/friend-socket.yaml
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
      silent: { messages: [] } # say nothing; the test sends ping
      unauthorized:
        message: { type: error, reason: unauthorized }
        close: { code: 4001, reason: unauthorized }
  status:
    match: { message: { type: get-status } } # runs on each matching message
    behaviors:
      online: { message: { id: '${ message.id }', status: online } }
      offline: { message: { id: '${ message.id }', status: offline } }
  subscribe:
    match: { message: { type: subscribe } }
    behaviors:
      updates:
        messages:
          - { message: { id: '${ message.id }', type: subscribed, topic: '${ message.topic }' } }
          - { message: { type: update, topic: '${ message.topic }', count: 1 }, delay: 1s }
```

- **Picking the Route:** the path matches as for HTTP (`{param}` captures a segment) and every
  listed `query` param must be present with that value (`'*'` means any value). If several Routes
  match, the one listing more `query` keys wins. Two Routes that could tie fail at startup. An
  upgrade no `ws` Route matches is handled as an HTTP GET: an HTTP Route on that path answers it,
  otherwise the HTTP miss (`501`). So does a repeated query key (`?service=a&service=b`) that would
  match two Routes equally.
- **`on: connect`:** at most one Case per Route. It takes no `match` and runs when the socket opens.
  Its templates see the upgrade request (`${ query.service }`). Without one, the server says
  nothing until a message arrives or the test calls `send`.
- **Message Cases** match each inbound text frame. `match` takes `message` (a deep partial match,
  like HTTP `body`) plus `pathParams`, `query`, `headers` and `cookies` of the upgrade, or one
  `${ }` predicate. A frame that parses as JSON is JSON, anything else stays a string:
  ``match: '${ message == `"ping"` }'``.
- **Unmatched message:** Decoy sends nothing, logs a miss, and keeps the socket open. Without a
  `pipeline`, a binary frame is always a miss.

Collections and Sessions work as for HTTP: `on: connect` Cases are entries like any other.

```yaml
# mocks/collections.yaml
- id: happy-path
  routes:
    - friend-socket:hello:ping
    - friend-socket:status:online
    - friend-socket:subscribe:updates
- id: manual-start
  from: happy-path
  routes:
    - friend-socket:hello:silent
```

## Behaviors

| field      | what it does                                                                                |
| ---------- | ------------------------------------------------------------------------------------------- |
| `message`  | one reply. A string goes out as-is; a mapping or list goes out as JSON text.                |
| `messages` | a list of `{ message, delay? }` items, each sent `delay` after the one before.              |
| `close`    | `{ code, reason }`: close the socket after the messages. `code` is `1000` or `3000`–`4999`. |
| `delay`    | wait before the first message.                                                              |

Use `message` or `messages`, not both. Every string is templated against the message envelope:
`{ path, pathParams, query, headers, cookies, message }`.

Each reply runs on its own timer. If a message arrives while an earlier reply still has delayed
items, both go out as their timers fire, interleaved. When the socket closes, pending items are
dropped.

## Sessions

A socket keeps the Session it opened with (`x-mock-session` on the upgrade). `useCollection` and
`useRoute` apply to its next message. An `on: connect` change applies to the Session's next socket.
Browsers can't set headers on `new WebSocket()`; under Playwright, the fixture's bridge adds it (see
[Playwright WebSockets](playwright.md#websockets)).

## Pushing from a test

[`send`](sessions-and-control.md#send) pushes a message to every open socket of the Route in the
caller's Session, skipping Cases:

```sh
curl -X POST localhost:8080/__decoy__/control/send \
  -H 'x-mock-session: test-42' \
  -H 'content-type: application/json' \
  -d '{"route":"friend-socket","message":"ping"}'
```

The [ws-node example](../../examples/ws-node) runs all of the above against a real client.

## Binary formats: plugin steps

Decoy has no built-in step for binary socket formats (msgpack envelopes, hand-rolled frames,
protobuf on a socket). A plugin step turns frames into JSON for the Cases and turns replies back into
frames, so Routes, Collections and Sessions work unchanged.

A plugin is a file that calls `definePlugin` from [`@decoyhq/plugin`](../../packages/plugin),
usually through a package's factory that takes typed options:

```ts
// plugins/binary-rpc.ts
import { binaryRpc } from '@acme/decoy-plugin-binary-rpc'

export default binaryRpc({
  topics: { 'users.get': { request: 'users.v1.GetUserRequest', reply: 'users.v1.User' } },
})
```

```yaml
# mocks/decoy.yaml
protos:
  roots: [../proto] # plugins use these through ctx.protobuf
plugins:
  rpc:
    command: node ../plugins/binary-rpc.ts # Decoy starts it; it serves until Decoy stops
pipelines:
  rpc: [rpc/frame] # <plugins key>/<step>
```

```yaml
# mocks/routes/rpc.yaml
id: rpc
transport: ws
match: { path: /ws/rpc }
pipeline: rpc # or inline: [rpc/frame]
cases:
  get-user:
    match:
      message: { kind: REQUEST, topic: users.get, payload: { id: u-1 } }
    behaviors:
      ann:
        message:
          kind: REPLY
          topic: users.get
          id: '${ message.id }'
          payload: { id: u-1, name: Ann }
```

- **Inbound:** each frame, text or binary, goes through the steps in order. Cases match the JSON the
  last one returns. A step can call `miss(reason)`: Decoy logs a miss and keeps the socket open.
- **Outbound:** each message goes through the steps in reverse as it is sent. The first step returns
  a string (sent as a text frame) or bytes (a binary frame), so one socket can mix both.
- **Control API `send`** goes through the same steps, per socket. A message that can't encode at
  all answers `400` with the plugin's error.
- **Startup:** a pipeline step no plugin serves fails startup. So does an outbound literal the step's
  `check` rejects, e.g. a typo in a protobuf payload next to a `${ }` template.
- **Failures:** a step that throws is an error event (stderr, TUI) naming the plugin, step, Route,
  connection, Session, the value and the plugin's stack; the socket stays open. A plugin that exits
  stops Decoy with its exit code and last stderr lines. Plugin stderr is shown as
  `[rpc: binary-rpc@0.1.0] …`.
- **Reload** applies Route changes; a changed `plugins:` entry needs a restart, and plugin `check`
  runs at startup only.

Other languages speak the [plugin protocol](../design/plugin-protocol.md) directly. The
[ws-plugin-node example](../../examples/ws-plugin-node) runs two plugins against real sockets.
