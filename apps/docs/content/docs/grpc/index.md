# gRPC

Decoy answers unary and server-streaming gRPC calls on its normal port. gRPC over HTTP/2 cleartext
(h2c), plain HTTP, and the Control API all share it. Point your gRPC client at Decoy instead of the
real service; no generated code or server stubs are needed. Client and bidi streaming are not
supported yet.
For gRPC over TLS, turn on [TLS](../tls/index.md); the same port then speaks gRPC over HTTPS.

## Proto roots

Decoy reads your `.proto` files at startup. List their import roots in `decoy.yaml`:

```yaml title="mocks/decoy.yaml"
routes: routes
collections: collections.yaml
port: 8080

protos:
  roots: [../proto] # import paths; every .proto under them is compiled
  # descriptorSets: [build/api.binpb]   # precompiled FileDescriptorSets, if compiling is awkward
```

Paths resolve relative to `decoy.yaml`. A missing root or a proto that doesn't compile stops startup
with the file and the reason.

If a root holds more than you need, such as a monorepo `proto/` folder with services that don't
compile together, list the entry files in `files`. Decoy compiles only those, and resolves their
imports through `roots`:

```yaml title="mocks/decoy.yaml (excerpt)"
protos:
  roots: [../proto]
  files: [../proto/services/catalog/**/*.proto] # `*` stays in one folder, `**` crosses folders
```

Each pattern must match at least one `.proto` under a root.

## gRPC Routes

```yaml title="mocks/routes/approvals.yaml"
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

```json title="gRPC request envelope"
{
  "service": "approvals.v1.Approvals",
  "method": "GetApproval",
  "metadata": { "x-user": "carol", "content-type": "application/grpc" },
  "message": { "id": "a-1" }
}
```

- By default, `message` is the decoded request as protobuf JSON: camelCase field names, enums as names, 64-bit
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

## Server streaming

A server-streaming method takes one request and answers with a `stream` of messages, each sent
`delay` after the one before it:

```yaml title="mocks/routes/approvals-watch.yaml"
id: approvals-watch
transport: grpc
match: { service: approvals.v1.Approvals, method: WatchApprovals }

cases:
  any:
    behaviors:
      progress:
        stream:
          - { payload: { id: '${ message.ids[0] }', state: PENDING } }
          - { payload: { id: '${ message.ids[0] }', state: APPROVED }, delay: 500ms }
        status: OK # end the call here

      gives-up:
        stream: [{ payload: { id: a-1, state: PENDING } }]
        status: ABORTED
        message: gave up

      live:
        stream: [{ payload: { id: a-1, state: PENDING } }] # no status: stays open
```

- Each `stream` item carries its data in `payload`.
- With `status`, the call ends after the list: `OK`, or an error with its `message` text and
  `metadata`, as for unary calls.
- Without `status`, the call stays open after the list. Each Control API
  [`send`](../sessions-and-control/index.md#send) to the Route from the call's Session arrives as another
  response message, until the client cancels. A `send` that comes while the list is still playing
  arrives after it. `{}` opens the stream with nothing scripted. Control API
  [`wait`](../sessions-and-control/index.md#wait) with `connected: true` resolves once such a stream is open.
- `message` outside the list is only the text of an error `status`, and `metadata` needs a
  `status` to be sent with. `stream` on a unary method fails at startup.
- `delay` on the Behavior itself waits before the first message.
- The call's request event is logged once, when the stream opens.

## Checked at startup

- The service and method exist in the loaded protos.
- Literal response `message` and `stream[].payload` values fit the method's output type: no
  unknown fields, valid enum names, scalar types and ranges. A message with no `${ }` is also
  parsed as protobuf JSON.
- Literal `match.message` values fit the input type in the envelope's form above. `created_at_ms`
  or `createdAtMs: 5` would never match a decoded request, so they fail: use JSON names, and write
  64-bit values as strings (`createdAtMs: '5'`). Well-known types must be written as the request
  renders them, too: an `Int64Value` as `'5'`, a `Timestamp` in UTC as `'2024-01-01T00:00:00Z'`.
  The error shows the form to use.
- Metadata names are lowercase header names and not `grpc-*`.
- Literal metadata values are printable ASCII (`0x20`–`0x7E`). A rendered value outside that range
  is dropped from the trailers with a warning.

For a named Pipeline, Transformers can change the shape of `message` before matching or
protobuf encoding. Decoy still validates the method and Behavior structure at startup.
A first output worker can check authored literals. The `protobuf` step checks actual
values against the descriptor when the Pipeline runs.

Templated values are checked when they render. A render or encode failure answers `INTERNAL` with
the Behavior address. A stream renders its whole list when the call comes in, so a failure there
answers `INTERNAL` before any message. A `send` message that doesn't fit the output type is
rejected with `400` and the stream stays open.

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

```js title="Client call with Session metadata"
const metadata = new grpc.Metadata()
metadata.set('x-mock-session', 'test-42')

client.getApproval({ id: 'a-1' }, metadata, callback)
```

Then switch that Session's Collection or Behaviors with the
[Control API](../sessions-and-control/index.md) over plain HTTP on the same port.

## Pipelines

A gRPC Route decodes calls with the default Pipeline `[grpc-frame, protobuf]`. An explicit
list replaces it. Decoy decodes in list order and encodes in reverse order. Each step can appear once, `grpc-frame` must come first
(gRPC bodies are always framed); `protobuf`, if present, follows it.
Without `protobuf`, the Route sees and answers raw bytes: `message` is a base64 string.

```yaml title="mocks/routes/approvals.yaml (excerpt)"
pipeline: [grpc-frame] # message is base64 bytes, e.g. `message: CgNhLTE=`
```

To add a Transformer, select a named Pipeline with `pipeline: approval-ids` on the Route.
For example, save this worker next to `decoy.yaml`. It normalizes IDs before matching and adds a prefix to each reply:

```js title="approval-ids.mjs"
import { defineTransformer } from '@decoyhq/transformer'

defineTransformer({
  name: 'approval-ids',
  version: '1',
  input: (request) => ({ ...request, id: request.id.toLowerCase() }),
  output: (reply) => ({ ...reply, id: 'ref-' + reply.id }),
})
```

```yaml title="decoy.yaml"
transformers:
  ids: { worker: [node, approval-ids.mjs] }

pipelines:
  approval-ids:
    input: [grpc-frame, protobuf, ids]
    output: [ids, protobuf, grpc-frame]
```

The explicit lists run in the order written. You can also write `approval-ids: [grpc-frame, protobuf, ids]`:
the [shorthand](../transformer-pipelines/index.md#use-a-shared-step-list) reverses the steps on output.
Input starts with the framed request bytes.
Here `grpc-frame` removes the frame, `protobuf` decodes the request using the method's
input type, and the Case sees the worker's result as `message`. Output starts with the
rendered reply. The worker changes it, `protobuf` encodes it using the method's output
type, and `grpc-frame` adds the frame the client expects. The same output steps apply to
each server-streamed reply and to Control API pushes. A nonempty output list must return wire bytes.
An omitted or empty direction uses the default gRPC codec and its startup protobuf validation.
An explicitly empty shared list (`approval-ids: []`) is different: Decoy warns at startup, uses
raw frame bytes as base64 for matching, and expects pre-framed reply bytes as base64. Decoy rejects incompatible adjacent built-ins at startup; a custom step can change the value type. An input command at the end of
the list must emit JSON for Case matching. A worker can return a JSON value or bytes,
which Cases see as a base64 string. A worker can call `miss` to skip its Route. If no
other Case matches, the client gets `UNIMPLEMENTED`. A named input step that fails
ends the call with `INVALID_ARGUMENT`. An authored reply that fails to encode gets `INTERNAL`.

See [`examples/grpc-node`](https://github.com/decoyhq/decoy/tree/main/examples/grpc-node) for a runnable Node client.
