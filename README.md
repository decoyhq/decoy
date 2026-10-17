# decoy

> A portable local mock tool: one native binary, YAML mocks, switchable scenarios.

Author mock **Routes** as YAML files, group their Behaviors into switchable **Collections**, and
point your app or tests at `decoy`. Fail-closed by default: a request with no matching mock gets a
diagnostic miss, never the real upstream.

## Status

Pre-alpha. The native Rust runtime (`crates/decoy-runtime`) serves HTTP today. Not published yet;
build from source.

## Quickstart

```sh
cargo build --release -p decoy-runtime
./target/release/decoy serve --routes mocks/routes --collections mocks/collections.yaml --port 8080
```

`mocks/routes/users.yaml`:

```yaml
id: users-by-id
transport: http
match:
  method: GET
  path: /users/{id}
cases:
  user-123:
    match:
      pathParams:
        id: "123"
    behaviors:
      success:
        status: 200
        body: { id: "123", name: Ada }
      missing:
        status: 404
        body: not found
```

`mocks/collections.yaml` (the first Collection is the default):

```yaml
- id: happy-path
  routes:
    - users-by-id:user-123:success
- id: user-missing
  from: happy-path
  routes:
    - users-by-id:user-123:missing
```

Switch Scenarios at runtime through the Control API under `/__decoy__/control`, per Session
(`x-mock-session` header). See [`docs/design/native-control-api-shape.md`](docs/design/native-control-api-shape.md).
Not yet mounted by `decoy serve` ([#194](https://github.com/what3verCODE/decoy/issues/194)).

## Roadmap

Playwright fixture → alpha release (binaries, npm wrapper, docs site) → richer matching
(query/headers/body, JMESPath, templating, delay) → gRPC → WebSocket/SSE → pipeline plugins.
See [`docs/design/next-direction.md`](docs/design/next-direction.md).

## Repository layout

```
crates/decoy-runtime/   native runtime and `decoy` CLI
packages/               language wrappers (e.g. Playwright), coming next
examples/               end-to-end user flows (`test:e2e`)
docs/adr/               accepted decisions
docs/design/            working design notes
CONTEXT.md              glossary
```
