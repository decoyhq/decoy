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
| `port`               | `--port <port>`          | `8080` (binds `127.0.0.1`)          |
| `ports`              | none                     | none                                |
| `collection`         | `--collection <id>`      | first Collection in the file        |
| `passthroughBaseUrl` | `--passthrough-base-url` | none (see [Passthrough](routes.md)) |
| `protos`             | none                     | none                                |
| `tls`                | none                     | none (plain http)                   |

`ports` opens extra named listeners. Each serves the same Routes, Sessions, and Control API as
`port`, so a gRPC client and an HTTP client can use different ports but share one Session:

```yaml
port: 8080
ports: { grpc: 50051 }
```

The keys are free names; they only label logs and errors. Every port speaks HTTP and gRPC.

`tls` serves every listener over HTTPS with your cert and key (PEM). ALPN picks HTTP/2 or HTTP/1.1
per connection, so gRPC over TLS works on the same port. A listener given as `{ port, tls: false }`
stays plain http, so you can mix both:

```yaml
tls:
  cert: certs/dev.pem
  key: certs/dev-key.pem
port: 8443 # https
ports:
  payments: 9443 # https
  legacy-crm: { port: 8080, tls: false } # http
```

`port` takes the same object form. `--port` changes only the port number. Clients must trust the
cert, e.g. `NODE_EXTRA_CA_CERTS=certs/dev.pem` for Node.

`tls: auto` skips the cert files: Decoy signs a localhost cert at startup with a local CA it creates
once in `.decoy/` next to the config (`ca.pem`, `ca-key.pem`) and reuses on later runs. Trust that
CA once (`NODE_EXTRA_CA_CERTS=mocks/.decoy/ca.pem`, grpc-js `credentials.createSsl(ca)`) and keep
`.decoy/` out of git.

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
