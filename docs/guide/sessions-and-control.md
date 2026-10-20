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

```sh
curl -X POST localhost:8080/__decoy__/control/useRoute \
  -H 'x-mock-session: test-42' \
  -H 'content-type: application/json' \
  -d '{"route":"users-by-id","case":"user-1","behavior":"grace"}'
```

## send

`POST /__decoy__/control/send` pushes a message to the live connections of a Route: today the open
[server-streaming gRPC calls](grpc.md#server-streaming).

```sh
curl -X POST localhost:8080/__decoy__/control/send \
  -H 'x-mock-session: test-42' \
  -H 'content-type: application/json' \
  -d '{"route":"approvals-watch","message":{"id":"a-1","state":"APPROVED"},"timeout":2000}'
```

- `message` is the response message as protobuf JSON; it goes through the Route's Pipeline. One
  that doesn't fit the method's output type gets `400` with the reason, and nothing is sent.
- Only connections opened in the same Session (`x-mock-session`) get it.
- With no live connection yet, `send` waits for one up to `timeout` milliseconds (default `5000`).
- A stream opened before a [reload](config.md#reloading) keeps the message type it opened with; a `send` that
  fits only the new type ends that stream with `INTERNAL`.

| status | when                                                           |
| ------ | -------------------------------------------------------------- |
| `200`  | `{"delivered":<n>}`: how many connections got the message      |
| `400`  | `message` doesn't encode for the Route                         |
| `404`  | unknown Route                                                  |
| `409`  | the Route has no long-lived connections (HTTP, unary gRPC)     |
| `408`  | no live connection of the Route in this Session within timeout |
| `503`  | Decoy is shutting down                                         |

Malformed JSON or an unknown body field returns `400` and a missing JSON content type `415`, as
for the other verbs.

## Health

`GET /__decoy__/health` → `200 {"status":"ok"}` while Decoy is serving. It is a liveness check
only: no diagnostics, and it never touches any Session.
