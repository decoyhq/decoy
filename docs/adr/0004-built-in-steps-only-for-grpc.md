# 0004. Named Transformers and gRPC built-ins

Date: 2026-10-22

## Status

Amends [ADR 0002](0002-pipeline-steps-and-plugin-workers.md). See the [Transport transformers PRD](../design/transport-transformers-prd.md).

## Context

WebSocket apps use application-specific framing, protobuf type selection, compression, and JSON normalization. A fixed list of codecs would require Decoy-specific configuration for each new combination. A single list also assumes input is the inverse of output.

## Decision

- Declare named `transformers` and `pipelines` in `decoy.yaml`. A Route selects a Pipeline. Each Pipeline has independently ordered `input` and `output` lists of Transformer references. Both lists run top to bottom.
- Use workers for persistent transforms and commands for one-shot transforms. One worker provides the configured directions and can keep state per connection. The SDK and language-neutral protocol use `input` and `output`. A command reads bytes to EOF on stdin and writes the result to stdout.
- Keep `grpc-frame` and `protobuf` as the built-in gRPC default typed by `service` and `method`. Put application-specific transforms in workers or commands. Workers can access Decoy's loaded protobuf schemas through host calls.
- Each transport defines its Pipeline boundaries. For WebSocket output, worker strings become text frames and bytes become binary frames. A terminal command produces binary frames. A WebSocket Route without a Pipeline keeps its normal text handling.

## Consequences

Routes can combine small commands with stateful workers without adding config syntax for a wire format. A list makes the order visible even when output transforms differ from input transforms. An SSE integration can use only `output`.
