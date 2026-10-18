# gRPC

Decoy answers unary gRPC calls on its normal port. gRPC over HTTP/2 cleartext (h2c), plain HTTP,
and the Control API all share it. Point your gRPC client at Decoy instead of the real service; no
generated code or server stubs are needed. Server, client, and bidi streaming are not supported yet.
For gRPC over TLS, turn on [TLS](tls.md); the same port then speaks gRPC over HTTPS.

## Proto roots

Decoy reads your `.proto` files at startup. List their import roots in `decoy.yaml`:

```yaml
# mocks/decoy.yaml
routes: routes
collections: collections.yaml
port: 8080
protos:
  roots: [../proto] # import paths; every .proto under them is compiled
  # descriptorSets: [build/api.binpb]   # precompiled FileDescriptorSets, if compiling is awkward
```

Paths resolve relative to `decoy.yaml`. A missing root or a proto that doesn't compile stops startup
with the file and the reason.

## gRPC Routes

```yaml
# mocks/routes/approvals.yaml
id: approvals-get
transport: grpc
match:
  service: approvals.v1.Approvals # fully qualified, as in the proto
  method: GetApproval
cases:
  pending:
    match:
      message: { id: a-1 }
    behaviors:
      ok:
        message: { id: a-1, state: PENDING, approvers: [alice] }
  any:
    behaviors:
      echo:
        message: { id: '${ message.id }', state: PENDING }
      not-found:
        status: NOT_FOUND
        message: 'approval ${ message.id } not found'
        metadata: { x-reason: missing }
      slow:
        delay: 2s
        message: { id: '${ message.id }', state: PENDING }
```

Collections, Sessions, and the Control API work exactly as for HTTP; addresses are still
`route:case:behavior`.

## The envelope

Cases and templates see one call as:

```json
{
  "service": "approvals.v1.Approvals",
  "method": "GetApproval",
  "metadata": { "x-user": "carol", "content-type": "application/grpc" },
  "message": { "id": "a-1" }
}
```

- `message` is the decoded request as protobuf JSON: camelCase field names, enums as names, 64-bit
  integers as strings, `bytes` as base64, and default values included (`id: ''`, `state:
STATE_UNSPECIFIED`).
- `metadata` is the request headers with lowercased names.

A Case `match` takes `message` (a deep partial match, like HTTP `body`) and `metadata` (like
`headers`, `'*'` for any value), or `${ }` predicates over the whole envelope.

## Behaviors

| Behavior                        | Answer                                                    |
| ------------------------------- | --------------------------------------------------------- |
| `message: { … }`                | `OK` with that response message                           |
| `status: NAME`, `message: text` | that gRPC status (`NOT_FOUND`, `UNAVAILABLE`, …) and text |
| `metadata: { name: value }`     | sent as trailers with either answer                       |
| `delay: 500ms`                  | waits before answering                                    |

Every string can be a `${ }` template, including `status`.

## Checked at startup

- The service and method exist in the loaded protos.
- Literal response `message` values fit the method's output type: no unknown fields, valid enum
  names, scalar types and ranges. A message with no `${ }` is also parsed as protobuf JSON.
- Literal `match.message` values fit the input type in the envelope's form above. `created_at_ms`
  or `createdAtMs: 5` would never match a decoded request, so they fail: use JSON names, and write
  64-bit values as strings (`createdAtMs: '5'`).
- Metadata names are lowercase header names and not `grpc-*`.

Templated values are checked when they render. A render or encode failure answers `INTERNAL` with
the Behavior address.

## Misses and errors

| Situation                         | Status             | Notes                                 |
| --------------------------------- | ------------------ | ------------------------------------- |
| no active Case matches the call   | `UNIMPLEMENTED`    | trailer `x-mock-miss: true`           |
| the call is compressed            | `INVALID_ARGUMENT` | turn off client compression           |
| the request body doesn't decode   | `INVALID_ARGUMENT` | message names the Route and the error |
| a Behavior can't render or encode | `INTERNAL`         | message names `route:case:behavior`   |

Nothing is forwarded to a real service: gRPC Routes have no Passthrough.

## Sessions

Send `x-mock-session` as call metadata. It's an ordinary gRPC header, so any client can set it:

```js
const metadata = new grpc.Metadata()
metadata.set('x-mock-session', 'test-42')
client.getApproval({ id: 'a-1' }, metadata, callback)
```

Then switch that Session's Collection or Behaviors with the
[Control API](sessions-and-control.md) over plain HTTP on the same port.

## Pipelines

A gRPC Route decodes calls with the default Pipeline `[grpc-frame, protobuf]`. An explicit
`pipeline` replaces it, steps in order. Each step can appear once, `grpc-frame` must come first
(gRPC bodies are always framed); `protobuf`, if present, follows it.
Without `protobuf`, the Route sees and answers raw bytes: `message` is a base64 string.

```yaml
pipeline: [grpc-frame] # message is base64 bytes, e.g. `message: CgNhLTE=`
```

See [`examples/grpc-node`](../../examples/grpc-node) for a runnable Node client.
