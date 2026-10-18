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
| `collection`         | `--collection <id>`      | first Collection in the file        |
| `passthroughBaseUrl` | `--passthrough-base-url` | none (see [Passthrough](routes.md)) |
| `protos`             | none                     | none                                |

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
missing proto roots, protos that don't compile, and an unknown startup Collection all fail at startup with a diagnostic.
