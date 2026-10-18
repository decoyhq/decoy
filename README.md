# decoy

> A portable local mock tool: one native binary, YAML mocks, switchable scenarios.

Author mock **Routes** as YAML files, group their Behaviors into switchable **Collections**, and
point your app or tests at `decoy`. Fail-closed by default: a request with no matching mock gets a
diagnostic miss, never the real upstream.

## Status

Alpha. The native Rust runtime (`crates/decoy-runtime`, binary in `crates/decoy`) serves HTTP today. Install with
`npm i -D @decoyhq/cli` or `curl -fsSL
https://github.com/decoyhq/decoy/releases/latest/download/install.sh | sh`; see
[Install](docs/guide/install.md).

Full docs: **[the guide](docs/guide/README.md)**: install, Routes, Collections, Sessions, config,
Playwright.

## Quickstart

```sh
cargo build --release -p decoy
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
        id: '123'
    behaviors:
      success:
        status: 200
        body: { id: '123', name: Ada }
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

Switch Scenarios at runtime through the Control API, per Session (`x-mock-session` header):

```sh
curl -X POST localhost:8080/__decoy__/control/useCollection -d '{"collection":"user-missing"}'
curl -i localhost:8080/users/123   # 404
curl -X POST localhost:8080/__decoy__/control/useCollection -d '{"collection":"happy-path"}'
```

Verbs: `useCollection`, `useRoute` (override one Behavior), `reset` (drop overrides), `endSession`
(end the Session). See
[`docs/design/native-control-api-shape.md`](docs/design/native-control-api-shape.md).

## Roadmap

Playwright fixture → alpha release (binaries, npm wrapper, docs site) → richer matching
(query/headers/body, JMESPath, templating, delay) → gRPC → WebSocket/SSE → pipeline plugins.
See [`docs/design/next-direction.md`](docs/design/next-direction.md).

## Repository layout

```
crates/decoy/           `decoy` CLI (binary)
crates/decoy-runtime/   native runtime library (engine + server)
crates/decoy-tui/       interactive terminal UI (in progress)
packages/               language wrappers (e.g. Playwright), coming next
examples/               end-to-end user flows (`test:e2e`)
docs/adr/               accepted decisions
docs/design/            working design notes
CONTEXT.md              glossary
```
