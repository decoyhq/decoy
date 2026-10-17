# Routes, Cases, and Behaviors

```txt
collection → route → case → behavior
```

A **Route** is one YAML file with an `id`, one `transport`, and a `match` saying _where_ the
interaction happens. Its **Cases** say _which request shape_ matched, and each Case owns named
**Behaviors**: what Decoy does.

```yaml
id: users-by-id # stable id; no ':' allowed
transport: http # the only transport today
match:
  method: GET # GET POST PUT PATCH DELETE HEAD OPTIONS
  path: /api/users/{id} # {name} captures one path segment
cases:
  user-1:
    match:
      pathParams:
        id: '1' # exact value; '*' matches any value
    behaviors:
      ada:
        status: 200
        body:
          id: '1'
          name: Ada Lovelace
      missing:
        status: 404
        body:
          error: user not found
```

Decoy loads every `.yaml`/`.yml` file under the routes directory, recursively, so you can group
files in folders. Duplicate Route ids fail startup.

## Case match

A Case with no `match` matches every request to its Route. Today a Case can match on `pathParams`.
Query, header, and body matching are on the roadmap.

## Response Behavior

The default `kind`. All fields are optional:

| field     | default | notes                                                                               |
| --------- | ------- | ----------------------------------------------------------------------------------- |
| `status`  | `200`   |                                                                                     |
| `headers` | none    | string map                                                                          |
| `body`    | empty   | object/array → JSON with `content-type: application/json` unless set; string → text |

## Passthrough Behavior

Forwards the matched request (method, path and query, headers, body) to a real upstream instead of
mocking it:

```yaml
id: profile
transport: http
match:
  method: GET
  path: /api/profile
cases:
  any:
    match: {}
    behaviors:
      upstream:
        kind: passthrough
        target:
          baseUrl: http://127.0.0.1:4174
```

The target is the first one set, in this order: the Behavior's `target`, the Route's top-level
`passthrough: { baseUrl }`, `decoy serve --passthrough-base-url`, then the request's own `Host`. If
forwarding fails, Decoy answers `502` with `{"error":"passthrough forwarding failed", ...}`, which is
never confused with a mock miss.

## Misses are fail-closed

A request no active Case matches gets `501` with `x-mock-miss: true` and
`{"error":"no active route case matched request"}`. Decoy never forwards a request unless a
Passthrough Behavior is selected.
