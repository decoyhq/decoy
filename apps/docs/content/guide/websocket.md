# WebSocket

Decoy accepts WebSocket upgrades on its normal port, next to plain HTTP, gRPC and the Control API.
Point the client's socket URL at Decoy. With [TLS](tls.md) on, the same port serves `wss://`.

Supported so far: text frames carrying JSON or plain text, `on: connect` Cases, timed replies,
`close`, Control API `send`, and binary formats through [Transformers](#transformers).

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
          - { payload: { id: '${ message.id }', type: subscribed, topic: '${ message.topic }' } }
          - { payload: { type: update, topic: '${ message.topic }', count: 1 }, delay: 1s }
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
| `messages` | a list of `{ payload, delay? }` items, each sent `delay` after the one before.              |
| `close`    | `{ code, reason }`: close the socket after the messages. `code` is `1000` or `3000`–`4999`. |
| `delay`    | initial wait, applied once before the Behavior sends anything.                              |

Use `message` or `messages`, not both. Every string is templated against the message envelope:
`{ path, pathParams, query, headers, cookies, message }`.

The Behavior-level `delay` works with either `message` or `messages`. An item-level `delay`
adds a wait before that item, measured from the previous send. On the first item, the two
delays add up. Both fields accept a nonnegative integer in milliseconds or a duration string with `ms` or `s`.

```yaml
updates:
  delay: 1000
  messages:
    - { payload: subscribed, delay: 250ms }
    - { payload: update, delay: 500ms }
```

Decoy sends `subscribed` after 1.25 seconds and `update` 0.5 seconds later.

Each reply runs on its own timer. If a message arrives while an earlier reply still has delayed
items, both go out as their timers fire, interleaved. When the socket closes, pending items are
dropped.

## Sessions

A socket keeps the Session it opened with (`x-mock-session` on the upgrade). `useCollection` and
`useRoute` apply to its next message. An `on: connect` change applies to the Session's next socket.
Browsers can't set headers on `new WebSocket()`; under Playwright, the fixture's bridge adds it (see
[Playwright WebSockets](playwright/websockets.md)).

## Pushing from a test

[`send`](sessions-and-control.md#send) pushes a message to every open socket of the Route in the
caller's Session, skipping Cases:

```sh
curl -X POST localhost:8080/__decoy__/control/send \
  -H 'x-mock-session: test-42' \
  -H 'content-type: application/json' \
  -d '{"route":"friend-socket","message":"ping"}'
```

The [ws-node example](../../../../examples/ws-node) runs all of the above against a real client.

## Transformers

Set `pipeline: <name>` on a WebSocket Route to select a [named Pipeline from your configuration](transformer-pipelines.md). Decoy runs
`input` before matching Cases and `output` after rendering a Behavior or when sending a Control API
message. Both directions run in the order listed.

On input, a command used as the last step must return JSON for Case matching. On output,
worker strings become text frames and bytes become binary frames.

The Control API `send` counts messages queued for delivery. The output Pipeline runs once per
recipient connection. If it fails, Decoy logs the Transformer and skips that frame.

During a transform, each socket can buffer up to 64 subsequent data frames totaling at most 16 MiB. Across all sockets, the process budgets 64 MiB for read-ahead, charged in whole KiB with a minimum of 1 KiB per frame. Exceeding a read-ahead limit closes the socket requesting more space.

Open sockets keep the Pipeline selected at upgrade. Protobuf host calls use the currently loaded descriptors, including after a reload. WebSocket request events capture the rendered Behavior reply before output transforms and delays. A captured reply can therefore differ from the frames that reach the client.
