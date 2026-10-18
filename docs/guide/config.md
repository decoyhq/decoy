# Configuration

`decoy serve` takes flags, a YAML config file, or both. Flags win over the file.

```yaml
# mocks/decoy.yaml
routes: routes # directory of Route files, relative to this file
collections: collections.yaml
port: 8080
```

```sh
decoy serve --config mocks/decoy.yaml
```

| file key             | flag                     | default                             |
| -------------------- | ------------------------ | ----------------------------------- |
| `routes`             | `--routes <dir>`         | required                            |
| `collections`        | `--collections <file>`   | required                            |
| `port`               | `--port <port>`          | `8080`                              |
| `host`               | `--host <host>`          | `localhost` (`127.0.0.1` and `::1`) |
| `listeners`          | none                     | none                                |
| `collection`         | `--collection <id>`      | first Collection in the file        |
| `passthroughBaseUrl` | `--passthrough-base-url` | none (see [Passthrough](routes.md)) |
| `protos`             | none                     | none                                |
| `tls`                | none                     | none (plain http)                   |

`listeners` opens extra named listeners. Each serves the same Routes, Sessions, and Control API as
`port`, so a gRPC client and an HTTP client can use different ports but share one Session:

```yaml
port: 8080
listeners: { grpc: 50051 }
```

The keys are free names; they only label logs and errors. Every listener speaks HTTP and gRPC.

`host` is where listeners bind. `localhost` (the default) binds both `127.0.0.1` and `::1`, so
clients work whichever one `localhost` resolves to for them. An IP binds just that address
(`0.0.0.0` for every interface, e.g. inside Docker); another name binds every address it resolves
to. A listener can set its own: `{ port: 9000, host: 0.0.0.0 }`. `--host` replaces the top-level
`host` and the main listener's.

`tls` serves every listener over HTTPS with your cert and key (PEM). ALPN picks HTTP/2 or HTTP/1.1
per connection, so gRPC over TLS works on the same port. A listener given as `{ port, tls: false }`
stays plain http, so you can mix both:

```yaml
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

```yaml
tls: auto
```

`protos` loads protobuf schemas for gRPC:

```yaml
protos:
  roots: [../proto] # import paths; every .proto under them is compiled at startup
  descriptorSets: [build/api.binpb] # optional precompiled FileDescriptorSets
```

`roots` work like protoc `-I` flags: imports resolve against them. If imports are written from the
top folder (`import "v1/keyboards/keyboards.proto";`), list that folder alone. If each folder
imports its neighbours by bare name (`import "keyboards.proto";`), list every such folder:

```yaml
protos:
  roots: [../proto, ../proto/v1/keyboards, ../proto/v2/my-entity]
```

Paths in the file resolve relative to the file; flag paths resolve relative to the working
directory. Unknown keys, unreadable files, invalid Routes or Collections, duplicate Route ids,
a port used twice or already taken, missing proto roots, protos that don't compile, unreadable or mismatched TLS cert/key, an unreadable `.decoy/` CA, and an unknown
startup Collection all fail at startup with a diagnostic.
