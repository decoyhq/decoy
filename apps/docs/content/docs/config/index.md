# Configuration

`decoy serve` takes flags, a YAML config file, or both. Flags win over the file.

If you've followed the [quick start](../quick-start/index.mdx), save this next to `collections.yaml` to keep the same settings without repeating the flags:

```yaml title="mocks/decoy.yaml"
routes: routes # directory of Route files, relative to this file
collections: collections.yaml
port: 8080
```

From the project directory, run:

```sh title="Start Decoy and watch for edits"
decoy serve --config mocks/decoy.yaml --watch
```

Add a one-off flag such as `--port 8081` to override the file without editing it.

## Settings

| file key             | flag                     | default                                                        |
| -------------------- | ------------------------ | -------------------------------------------------------------- |
| `routes`             | `--routes <dir>`         | required                                                       |
| `collections`        | `--collections <file>`   | required                                                       |
| `port`               | `--port <port>`          | `8080`                                                         |
| `host`               | `--host <host>`          | `localhost` (`127.0.0.1` and `::1`)                            |
| `listeners`          | none                     | none                                                           |
| `collection`         | `--collection <id>`      | first Collection in the file                                   |
| `passthroughBaseUrl` | `--passthrough-base-url` | none (see [Passthrough](../routes/index.md))                   |
| `protos`             | none                     | none                                                           |
| `tls`                | none                     | none (plain http)                                              |
| `maxBodyBytes`       | none                     | `33554432` (32 MiB)                                            |
| `transformers`       | none                     | none (see [Pipeline setup](../transformer-pipelines/index.md)) |
| `pipelines`          | none                     | none (see [Pipeline setup](../transformer-pipelines/index.md)) |

`listeners` opens extra named listeners. Each serves the same Routes, Sessions, and Control API as
`port`, so a gRPC client and an HTTP client can use different ports but share one Session:

```yaml title="decoy.yaml (excerpt)"
port: 8080
listeners: { grpc: 50051 }
```

The keys are free names; they only label logs and errors. Every listener speaks HTTP and gRPC.

`host` is where listeners bind. `localhost` (the default) binds both `127.0.0.1` and `::1`, so
clients work whichever one `localhost` resolves to for them. An IP binds just that address
(`0.0.0.0` for every interface, e.g. inside Docker); another name binds every address it resolves
to. A listener can set its own: `{ port: 9000, host: 0.0.0.0 }`. `--host` replaces the top-level
`host` and the main listener's. Ports must be unique across listeners, whatever their host.

`tls` serves every listener over HTTPS with your cert and key (PEM). ALPN picks HTTP/2 or HTTP/1.1
per connection, so gRPC over TLS works on the same port. A listener given as `{ port, tls: false }`
stays plain http, so you can mix both:

```yaml title="decoy.yaml (excerpt)"
tls:
  cert: certs/dev.pem
  key: certs/dev-key.pem

port: 8443 # https
listeners:
  payments: 9443 # https
  legacy-crm: { port: 8080, tls: false } # http
```

`port` takes the same object form (`{ port, host, tls }`). `--port` changes only the port number. Clients must trust the
cert, e.g. `NODE_EXTRA_CA_CERTS=certs/dev.pem` for Node.

`tls: auto` skips the cert files: Decoy signs a fresh localhost cert (valid a year) at every start with a
local CA kept in `.decoy/` next to the config (`ca.pem`, `ca-key.pem`). The CA is created once and
reused, so trust it once (`NODE_EXTRA_CA_CERTS=mocks/.decoy/ca.pem`, grpc-js
`credentials.createSsl(ca)`). `ca.pem` is rebuilt from the key if it goes missing. Decoy writes a
`.gitignore` into `.decoy/` so the CA key stays out of git.

```yaml title="decoy.yaml (excerpt)"
tls: auto
```

`protos` loads protobuf schemas for gRPC:

```yaml title="decoy.yaml (excerpt)"
protos:
  roots: [../proto] # import paths; every .proto under them is compiled at startup
  descriptorSets: [build/api.binpb] # optional precompiled FileDescriptorSets
```

`files` compiles only the matching entry files instead of every `.proto` under the roots. Their
imports still resolve through `roots`. Each pattern must match at least one `.proto` under a root:

```yaml title="decoy.yaml (excerpt)"
protos:
  roots: [../proto]
  files: [../proto/services/catalog/**/*.proto]
```

`roots` work like protoc `-I` flags: imports resolve against them. If imports are written from the
top folder (`import "v1/keyboards/keyboards.proto";`), list that folder alone. If each folder
imports its neighbours by bare name (`import "keyboards.proto";`), list every such folder:

```yaml title="decoy.yaml (excerpt)"
protos:
  roots: [../proto, ../proto/v1/keyboards, ../proto/v2/my-entity]
```

## Transformers and Pipelines

Use the top-level `transformers` and `pipelines` keys to define reusable steps and name their input and output order. HTTP, gRPC, and WebSocket Routes can all select a named Pipeline. See [Configure a Transformer Pipeline](../transformer-pipelines/index.md) for setup and transport-specific links.

## Startup validation

Paths in the file resolve relative to the file; flag paths resolve relative to the working
directory. Unknown keys in the config, Routes or Collections, unreadable files, invalid Routes or Collections, duplicate Route or Collection ids,
a port used twice or already taken, missing proto roots, a proto `files` pattern that matches nothing, protos that don't compile, unreadable or mismatched TLS cert/key, an unreadable `.decoy/` CA, an unknown
startup Collection, a worker that doesn't start, an unknown Transformer or unsupported Pipeline direction, and an outbound
literal a worker's `check` rejects all fail at startup with a diagnostic.

## Reloading

`--watch` (on `decoy serve` and on bare `decoy`) reloads when the config file, the collections file,
a Route file, a `.proto` file under a proto root (with `files`: a picked file, a file it imports, or a new file the
patterns match) or a descriptor set changes. In the terminal UI
that bare `decoy` opens, `/reload` does the same on demand.

```sh title="Start Decoy and watch for edits"
decoy serve --config mocks/decoy.yaml --watch
```

A reload re-reads everything and swaps it in while the server keeps running; open connections stay
open and requests already running finish on the old config. Selections survive: an override whose
Behavior is gone is dropped with a warning, a Session whose Collection is gone falls back to the
default `collection`, and Sessions that never picked a Collection follow a new default. If a file
doesn't parse or validate, the old config stays live and the error is printed once per save.
`port`, `host`, `listeners` and `tls` need a restart; changing them prints
`listener changes need restart`. Changes to Transformer worker definitions also need a restart
and print `Transformer worker changes need restart`. When the config points `routes`,
`collections` or `protos` somewhere else, the watch follows it.

Changes are picked up within about a second of the last write. On network filesystems and on
Windows drives seen from WSL2 (`/mnt/c/...`) file events don't arrive, so Decoy polls the files
there instead; `--watch-poll` forces polling anywhere (and implies `--watch`).

## Limits and shutdown

- Request bodies over `maxBodyBytes` (default `33554432`, 32 MiB) get `413` (gRPC:
  `RESOURCE_EXHAUSTED`) instead of being buffered. Raise it for large uploads:
  `maxBodyBytes: 104857600`.
- Ctrl-C or SIGTERM stops `decoy serve`: it stops accepting, gives open requests 5s to finish (a
  long `delay` or a slow passthrough doesn't hold it up longer), then exits with status 0.
