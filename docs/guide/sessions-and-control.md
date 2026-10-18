# Sessions and the Control API

## Sessions

Every request belongs to a **Session**, named by its `x-mock-session` header. Requests without the
header use the default Session. Each Session has its own **Selection**: active Collection plus
per-Route overrides. So parallel tests can share one `decoy` without seeing each other's changes.
New Sessions start on the default Collection.

## Control API

Command-shaped `POST` endpoints under the reserved `/__decoy__` path. Pick the Session with
`x-mock-session`; leave it out for the default Session.

| verb            | request                                 | body                                                                  |
| --------------- | --------------------------------------- | --------------------------------------------------------------------- |
| `useCollection` | `POST /__decoy__/control/useCollection` | `{"collection":"no-users"}`                                           |
| `useRoute`      | `POST /__decoy__/control/useRoute`      | `{"route":"users-by-id","case":"user-1","behavior":"grace"}`          |
| `reset`         | `POST /__decoy__/control/reset`         | empty: drops the Session's route overrides                            |
| `endSession`    | `POST /__decoy__/control/endSession`    | empty: ends the Session; it starts again from defaults when next used |

Success returns `200` with the Session's Selection. Unknown Collections or Behavior addresses and
malformed JSON return `400 {"error":"..."}`, and unknown `/__decoy__` paths return `404`.

```sh
curl -X POST localhost:8080/__decoy__/control/useRoute \
  -H 'x-mock-session: test-42' \
  -d '{"route":"users-by-id","case":"user-1","behavior":"grace"}'
```

## Health

`GET /__decoy__/health` → `200 {"status":"ok"}` while Decoy is serving. It is a liveness check
only: no diagnostics, and it never touches any Session.
