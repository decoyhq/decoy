# 0004. Give named Pipelines independent input and output steps

Accepted 2026-10-22. Amends [ADR 0002](0002-pipeline-steps-and-plugin-workers.md).
[ADR 0006](0006-shared-pipeline-step-lists.md) adds a shared step list without changing the
execution order of explicit direction lists.

A WebSocket client can send compressed binary requests and receive text replies. Reversing the
input steps would force the reply through transforms it does not need. Adding a built-in for
each application format would also tie new formats to Decoy releases.

A Route can select a named Pipeline with separate `input` and `output` lists. Each list runs in
the order written. A Transformer can use a persistent worker for per-connection state or protobuf
host calls, or a one-shot command for an executable that reads stdin and writes stdout. The
built-in gRPC steps remain `grpc-frame` and `protobuf`.

The transport owns framing. On WebSocket output, a worker can return text or bytes to choose the
frame kind. A terminal command produces bytes. This lets applications combine existing tools and
stateful workers while keeping wire formats out of Cases. See the [original proposal](https://github.com/decoyhq/decoy/pull/415)
and the [worker protocol](../../apps/docs/content/docs/transformer-worker-protocol/index.md).
