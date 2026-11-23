# Sessions and the Control API

## Sessions

Every request belongs to a **Session**, named by its `x-mock-session` header. Requests without the
header use the default Session. Each Session has its own **Selection**: active Collection plus
per-Route overrides. So parallel tests can share one `decoy` without seeing each other's changes.
New Sessions start on the default Collection. Decoy holds up to 10,000 Sessions with a
Selection. Past that, the one changed least recently is ended as if it had called `endSession`,
with a warning; the default Session is never ended this way.

## Control API

Command-shaped `POST` endpoints under the reserved `/__decoy__` path. Pick the Session with
`x-mock-session`; leave it out for the default Session. Every call, `reset` and `endSession`
too, sends `content-type: application/json`; anything else gets `415`, so a web page on another
site can't switch your Scenarios with a form or a simple `fetch`.

| verb            | request                                 | body                                                                          |
| --------------- | --------------------------------------- | ----------------------------------------------------------------------------- |
| `useCollection` | `POST /__decoy__/control/useCollection` | `{"collection":"no-users"}`                                                   |
| `useRoute`      | `POST /__decoy__/control/useRoute`      | `{"route":"users-by-id","case":"user-1","behavior":"grace"}`                  |
| `reset`         | `POST /__decoy__/control/reset`         | empty or `{}`: drops the Session's route overrides                            |
| `endSession`    | `POST /__decoy__/control/endSession`    | empty or `{}`: ends the Session; it starts again from defaults when next used |

`useRoute` swaps the Behavior of that `route:case` where it sits in the active Collection, so
more specific Cases listed after it still win. A `route:case` the Collection doesn't list goes
after all of its entries, and among those the last `useRoute` call wins.

Success returns `200` with the Session's Selection. Unknown Collections or Behavior addresses,
malformed JSON and unknown body fields return `400 {"error":"..."}`, a missing JSON content type
returns `415`, and unknown `/__decoy__` paths return `404`.

```sh title="Override a Behavior in test-42"
curl -X POST localhost:8080/__decoy__/control/useRoute \
  -H 'x-mock-session: test-42' \
  -H 'content-type: application/json' \
  -d '{"route":"users-by-id","case":"user-1","behavior":"grace"}'
```

## send

`POST /__decoy__/control/send` pushes a message to the live connections of a Route: open
[server-streaming gRPC calls](grpc.md#server-streaming) and [WebSocket](websocket.md) sockets.

```sh title="Push a gRPC message in test-42"
curl -X POST localhost:8080/__decoy__/control/send \
  -H 'x-mock-session: test-42' \
  -H 'content-type: application/json' \
  -d '{"route":"approvals-watch","message":{"id":"a-1","state":"APPROVED"},"timeout":2000}'
```

- gRPC: `message` is the response message as protobuf JSON; it goes through the Route's Pipeline.
  One that doesn't fit the method's output type gets `400` with the reason, and nothing is sent.
- WebSocket: the Route’s output Pipeline transforms each recipient’s message, including strings. Without output steps, strings go out as-is and other values become JSON text.
- Only connections opened in the same Session (`x-mock-session`) get it.
- With no live connection yet, `send` waits for one up to `timeout` milliseconds (default `5000`,
  at most 5 minutes: a longer `timeout` is cut to that).
- Each connection queues up to 64 unread messages. A connection whose queue is full skips the
  message and doesn't count in `delivered`; if every one is full, `send` waits as above.
- After a [reload](config.md#reloading), `message` is encoded with the current output type, also for
  streams opened before it; an older client skips fields it doesn't know. A stream whose Route now
  answers with a different message type doesn't get it.

| status | when                                                                                                   |
| ------ | ------------------------------------------------------------------------------------------------------ |
| `200`  | `{"delivered":<n>}`: how many connections got the message                                              |
| `400`  | `message` doesn't encode for the Route                                                                 |
| `404`  | unknown Route                                                                                          |
| `409`  | the Route has no long-lived connections (HTTP, unary gRPC)                                             |
| `408`  | no live connection of the Route in this Session took it within timeout (none, all full, or older type) |
| `503`  | Decoy is shutting down                                                                                 |

Malformed JSON or an unknown body field returns `400` and a missing JSON content type `415`, as
for the other verbs.

## wait

`POST /__decoy__/control/wait` with `connected: true` returns once the caller's Session has a live
connection of the Route, so a test can `send` knowing the page's socket or stream is open.

```sh title="Wait for a connection in test-42"
curl -X POST localhost:8080/__decoy__/control/wait \
  -H 'x-mock-session: test-42' \
  -H 'content-type: application/json' \
  -d '{"route":"friend-socket","connected":true,"timeout":2000}'
```

- Only connections opened in the same Session (`x-mock-session`) count, and only those `send`
  can reach: a WebSocket socket always counts; a gRPC stream counts only while it stays open for
  `send` (its Case has no `status`) and was opened with the Route's current output type. A finite
  scripted stream (its Case ends with a `status`) is never registered, so `wait` answers `408`.
- It waits up to `timeout` milliseconds (default `5000`, at most 5 minutes).
- `connected: true` is the only condition so far.

| status | when                                                                      |
| ------ | ------------------------------------------------------------------------- |
| `200`  | `{"connections":<n>}`: how many such connections are open in this Session |
| `400`  | `connected` missing or not `true`, or another condition (`message`)       |
| `404`  | unknown Route                                                             |
| `409`  | the Route has no long-lived connections (HTTP, unary gRPC)                |
| `408`  | no live connection of the Route opened in this Session within timeout     |
| `503`  | Decoy is shutting down                                                    |

Malformed JSON or an unknown body field returns `400` and a missing JSON content type `415`, as
for the other verbs.

## Health

`GET /__decoy__/health` → `200 {"status":"ok"}` while Decoy is serving. It is a liveness check
only: no diagnostics, and it never touches any Session.
