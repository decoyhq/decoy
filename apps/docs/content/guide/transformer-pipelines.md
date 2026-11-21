# Configure a Transformer Pipeline

Define a Transformer once in `decoy.yaml` to reuse it across named Pipelines. In the example below,
`gzip` runs a command and `approvals` uses a persistent worker. The `approvals-feed` Pipeline
decompresses incoming data before passing it to the worker. On output, it runs the worker first
and compresses the result.

```yaml
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

## Route support

Choose the configuration for your Route’s transport.

| Route transport                        | Supported configuration                                                                                          |
| -------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| [WebSocket](websocket.md#transformers) | Set `pipeline: approvals-feed` to run the named Pipeline on this Route. |
| [gRPC](grpc.md#pipelines) | Use a named Pipeline to add Transformers to the built-in `grpc-frame` and `protobuf` steps. |
| [HTTP](routes.md#http-body-pipelines) | Set `pipeline: approvals-feed` to transform the request and response bodies. |

Several compatible Routes can share a named Pipeline.

## Validation and execution limits

Decoy validates referenced Transformers and directions when loading Routes. A worker can also
check the authored output literal at startup when it is the first output step. Restart Decoy to
apply worker config changes.

A one-shot Transformer command has 5 seconds to finish. Decoy fails the transform if stdout exceeds 16 MiB or stderr exceeds 64 KiB. Up to eight one-shot commands can run across all connections. When all slots are busy, the next command fails the transform before spawning.
