# Configure a Transformer Pipeline

Define a Transformer once in `decoy.yaml` to reuse it across named Pipelines. In the example below,
`gzip` runs a command and `approvals` uses a persistent worker. The `approvals-feed` Pipeline
decompresses incoming data before passing it to the worker. On output, it runs the worker first
and compresses the result.

```yaml title="decoy.yaml (excerpt)"
transformers:
  approvals:
    worker: [node, ../workers/approvals.ts]

  gzip:
    input: [gzip, -dc]
    output: [gzip, -c]

pipelines:
  approvals-feed:
    input: [gzip, approvals]
    output: [approvals, gzip]
```

## Select a Pipeline on a Route

Set `pipeline` alongside `id`, `transport`, and `match` in the Route file. Both directions run in the listed order. Several compatible Routes can share one named Pipeline.

```yaml title="Route excerpt: select a named Pipeline"
pipeline: approvals-feed
```

| Transport                                              | What the Pipeline transforms                                                                                                                    |
| ------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| [HTTP](../routes/index.md#http-body-pipelines)         | Request and response bodies.                                                                                                                    |
| [gRPC](../grpc/index.md#pipelines)                     | Framed request bytes and replies. Include `grpc-frame` and `protobuf` in the appropriate order if your Transformer works with decoded messages. |
| [WebSocket](../websocket/index.md#websocket-pipelines) | Incoming frame payloads and outgoing messages, including Control API pushes.                                                                    |

In a named gRPC Pipeline, a configured Transformer takes precedence over a built-in step with the same name. For working configurations, see the [shared HTTP and WebSocket example](https://github.com/decoyhq/decoy/tree/main/examples/ws-transformer-node) and the [gRPC example](https://github.com/decoyhq/decoy/tree/main/examples/grpc-node).

## Validation and execution limits

Decoy validates referenced Transformers and directions when loading Routes. A worker can also
check the authored output literal at startup when it is the first output step. Restart Decoy to
apply worker config changes.

A one-shot Transformer command has 5 seconds to finish. Decoy fails the transform if stdout exceeds 16 MiB or stderr exceeds 64 KiB. At most eight one-shot commands can run at once in the Decoy process. When all slots are busy, the next command fails the transform before spawning.
