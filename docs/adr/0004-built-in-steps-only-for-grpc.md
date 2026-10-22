# 0004. Built-in pipeline steps only for gRPC; other formats are plugins

Date: 2026-10-22

## Status

Accepted. Amends ADR 0002: its "Built-in steps first" bullet listed `msgpack` and `json` as well,
its plugins were declared by "manifest + executable" (now `command` only), and it had built-in
steps working before the plugin worker contract (now the reverse).

## Context

ADR 0002 planned `grpc-frame`, `protobuf`, `msgpack` and `json` as built-in steps. Designing
WebSocket (#218, `docs/design/transports.md`) against real sockets showed that the binary formats
apps put on a socket are app-specific: a msgpack map naming its proto type, a hand-rolled binary
header in front of a protobuf payload, compression around either. Each one asked Decoy for more
config: `protobuf` type options (`typeFrom`, `typeBy`, a `${ }` key), per-frame-kind pipelines, and
a declarative `frame` step. That config grew with every new format and still couldn't describe the
next one.

## Decision

- **Decoy ships only `grpc-frame` and `protobuf`**, as the gRPC default pipeline, typed by the
  Route's `service`/`method`. No `json`, `msgpack`, or declarative framing step.
- **Every other wire format is a plugin step** (ADR 0002 plugin workers). Plugins reach the protos
  Decoy already compiled through SDK host calls, so they don't load `.proto` files themselves.
- **Without a pipeline**, a WebSocket text frame is JSON when it parses and a string otherwise, and a
  binary frame is a MISS. This is the transport's default, not a step.
- **Frame kind belongs to the plugin:** it reads `text`/`binary` inbound and picks it outbound by
  returning a string or bytes. Config has no `frame` key.
- **Plugin failures are loud.** A failed call is an error event with everything needed to fix it;
  the plugin process exiting stops Decoy. Decoy doesn't retry or restart.

## Consequences

- Raw protobuf over WebSocket, which a built-in `protobuf` step with `in`/`out` could have covered,
  needs a small plugin.
- The plugin worker and the `@decoyhq/plugin` SDK must land before any binary WebSocket format can
  be mocked. JSON and text sockets work without them.
- Decoy's config surface stays fixed; formats grow in user code, where they can reuse the app's own
  pack/unpack logic.
- Built-in steps can still be added later if one format turns out common across many users.
