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

A Case with no `match` matches every request to its Route. Otherwise `match` is a mapping of
fields, and **all** fields must match:

```yaml
cases:
  vip:
    match:
      pathParams: { id: '*' } # '*' = present, any value
      query: { priority: high } # ?priority=high (other params ignored)
      headers: { X-Tier: '*' } # header names are case-insensitive
      cookies: { sid: abc }
      body: { items: [{ sku: gold }] } # deep partial match
```

| field                                       | how it matches                                                                                                                                                      |
| ------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `pathParams`, `query`, `headers`, `cookies` | each listed key must be present; values compare as strings (`page: 5` matches `?page=5`); a repeated query key matches if any value does                            |
| `body`                                      | objects: listed keys must match, others are ignored. Arrays: the request array must contain each listed element, in any order. Scalars: exactly equal (`1` ≠ `"1"`) |

`body` is the parsed JSON when the request's `content-type` is JSON (`application/json`,
`*+json`), otherwise the raw text. Repeated request headers are joined with `", "`. An unknown
field, such as a misspelled `querry`, fails startup.

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
