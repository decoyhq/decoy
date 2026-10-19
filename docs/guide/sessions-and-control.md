# Sessions and the Control API

## Sessions

Every request belongs to a **Session**, named by its `x-mock-session` header. Requests without the
header use the default Session. Each Session has its own **Selection**: active Collection plus
per-Route overrides. So parallel tests can share one `decoy` without seeing each other's changes.
New Sessions start on the default Collection.

## Control API

Command-shaped `POST` endpoints under the reserved `/__decoy__` path. Pick the Session with
`x-mock-session`; leave it out for the default Session. Every call, `reset` and `endSession`
too, sends `content-type: application/json`; anything else gets `415`, so a web page on another
site can't switch your Scenarios with a form or a simple `fetch`.

| verb            | request                                 | body                                                                  |
| --------------- | --------------------------------------- | --------------------------------------------------------------------- |
| `useCollection` | `POST /__decoy__/control/useCollection` | `{"collection":"no-users"}`                                           |
| `useRoute`      | `POST /__decoy__/control/useRoute`      | `{"route":"users-by-id","case":"user-1","behavior":"grace"}`          |
| `reset`         | `POST /__decoy__/control/reset`         | empty: drops the Session's route overrides                            |
| `endSession`    | `POST /__decoy__/control/endSession`    | empty: ends the Session; it starts again from defaults when next used |

`useRoute` swaps the Behavior of that `route:case` where it sits in the active Collection, so
more specific Cases listed after it still win. A `route:case` the Collection doesn't list goes
after all of its entries, and among those the last `useRoute` call wins.

Success returns `200` with the Session's Selection. Unknown Collections or Behavior addresses and
malformed JSON return `400 {"error":"..."}`, a missing JSON content type returns `415`, and unknown
`/__decoy__` paths return `404`.

```sh
curl -X POST localhost:8080/__decoy__/control/useRoute \
  -H 'x-mock-session: test-42' \
  -H 'content-type: application/json' \
  -d '{"route":"users-by-id","case":"user-1","behavior":"grace"}'
```

## Health

`GET /__decoy__/health` → `200 {"status":"ok"}` while Decoy is serving. It is a liveness check
only: no diagnostics, and it never touches any Session.
