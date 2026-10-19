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

Path segments and `pathParams` compare percent-decoded, like `query` (`/users/a%20b` gives `id: a
b`; `%2F` stays inside one segment). The envelope `path` stays as sent.

`body` is the parsed JSON when the request's `content-type` is JSON (`application/json`,
`*+json`), otherwise the raw text. Repeated request headers are joined with `", "`. An unknown
field, such as a misspelled `querry`, fails startup.

### Predicates

When a literal pattern isn't enough, use a [JMESPath](https://jmespath.org) predicate: a string
that is exactly `${ <expression> }`. It can replace the whole `match` or any single field, and it
always sees the whole request:

```yaml
cases:
  get-user: # GraphQL: same path, different operation
    match:
      headers: { x-client: web } # literal fields and predicates are ANDed
      body: "${ body.operationName == 'GetUser' }"
  big-order:
    match: '${ body.variables.amount > `1000` }'
```

The **request envelope**, what predicates and templates see:

```yaml
method: POST
url: http://127.0.0.1:8080/graphql?verbose=true
path: /graphql
pathParams: {}
query: { verbose: 'true' } # a repeated key becomes a list: { tag: [a, b] }
headers: { content-type: application/json, x-client: web } # names lowercased
cookies: { sid: abc }
body: { operationName: GetUser } # parsed JSON, or text, or null
```

Truthiness follows JMESPath: `null`, `false`, `""`, `[]`, and `{}` don't match, and anything else
does. Literals inside expressions use backticks (`` `1000` ``) or quotes (`'GetUser'`). Header names
with dashes need quotes: `headers."x-tier"`. Invalid expressions fail startup, naming the
`route:case`.

## Response Behavior

The default `kind`. All fields are optional:

| field     | default | notes                                                                               |
| --------- | ------- | ----------------------------------------------------------------------------------- |
| `status`  | `200`   |                                                                                     |
| `headers` | none    | string map                                                                          |
| `body`    | empty   | object/array → JSON with `content-type: application/json` unless set; string → text |

### Delay

Any Behavior, Response or Passthrough, can wait before answering: `delay: 250` (milliseconds) or
a duration such as `500ms`, `2s`, `1m 30s`. Other requests are not held up while one waits.

```yaml
behaviors:
  slow:
    delay: 2s
    status: 504
```

### Templates

Any string in `status`, `headers`, or `body`, at any depth, can use `${ <jmespath> }` over the same
request shape as [predicates](#predicates). Keys are never templated.

```yaml
behaviors:
  echo:
    status: '${ query.status || `201` }'
    headers:
      x-user: 'user-${ pathParams.id }'
    body:
      id: '${ pathParams.id }' # exactly one ${ } keeps the type: here a string
      age: '${ body.age }' # …here a number
      greeting: 'Hello, ${ body.name }!' # mixed text interpolates (null → "")
      requestId: '${ uuid() }'
      literal: '\${ not a template }' # \${ is a literal ${
```

A templated `status` must render an integer from 100 to 599; otherwise the request gets a `500`
explaining which Behavior failed. Invalid expressions fail startup, naming the `route:case:behavior`.
Behaviors without `${` are served as written.

Built-in functions, next to [JMESPath's own](https://jmespath.org/specification.html#built-in-functions):

| function                | returns                                       |
| ----------------------- | --------------------------------------------- |
| `uuid()`                | random v4 UUID string                         |
| `now()` / `now('unix')` | RFC 3339 UTC timestamp / seconds since epoch  |
| `random_int(min, max)`  | random integer, both bounds inclusive         |
| `random_item(array)`    | one random element, `null` for an empty array |

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
forwarding fails, Decoy answers `502` with `{"error":"passthrough forwarding failed", ...}`, which
is never confused with a mock miss. An upstream that doesn't connect within 10s or answer within 30s
gets `504`. A request that comes back to the same Decoy (e.g. the request's own `Host` is Decoy)
gets `508` instead of looping: each Decoy adds its id to `x-decoy-via`. `x-mock-session` is not
forwarded, so a Decoy further down a chain answers from its default Session. Every repeated response
header (such as `set-cookie`) comes back.

## Misses are fail-closed

A request no active Case matches gets `501` with `x-mock-miss: true` and a body that says why each
active route entry, checked bottom-to-top, did not match:

```json
{
  "error": "no active route case matched request",
  "candidates": [
    {
      "address": "get-user:user-123:success",
      "reason": "condition",
      "field": "pathParams.id",
      "expected": "123",
      "actual": "456"
    },
    { "address": "list-users:any:ok", "reason": "path", "expected": "/users" }
  ]
}
```

`reason` is one of `method`, `path` (with the Route's `expected` value), `condition` (the first
failed Case condition: `field`, `expected`, and the request's `actual`, `null` when absent; `field`
is `match` for a whole-Case predicate, and a predicate that failed to evaluate adds its `error`),
`decode` (a gRPC request the Route's Pipeline could not
decode, with the `error`), `transport`, `missingRoute`, `missingCase`, or `missingBehavior`. Decoy never forwards a request unless a Passthrough Behavior is selected.
