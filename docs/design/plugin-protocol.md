# Plugin wire protocol, version 1

This spec defines the worker protocol for the Decoy runtime and the `@decoyhq/plugin` SDK. The
[Transport Transformers PRD](transport-transformers-prd.md) describes how to configure workers.

## Process

- Decoy starts one process per `transformers` entry with a `worker` argv list, in the directory of
  `decoy.yaml`, with `DECOY_PLUGIN=1` added to the environment.
- stdin and stdout carry the protocol and nothing else: anything else on stdout is a protocol error
  that ends the plugin, so an SDK sends console output to stderr. stderr is free text: Decoy forwards each line
  as `[<key>: <name>@<version>] <line>` and keeps the last 20 lines for exit reports.
- The process lives as long as Decoy. Decoy sends `shutdown` when it stops and kills the process if it
  is still running 2 s later. If the process exits on its own, Decoy reports the exit code and stderr
  tail and exits non-zero. Decoy never restarts a plugin, not even on a `--watch` reload.

## Framing

Each message is a 4-byte big-endian length, then that many bytes of one msgpack map. A message is at
most 16 MiB; a longer length ends the plugin as a protocol error. Decoy never sends a longer one: a
`call` that wouldn't fit fails as a plugin error for that frame only. Every map has a string `kind`.

## Values

Values are msgpack. Wire bytes are msgpack `bin`; everything else is the JSON data model (nil, bool,
integer, float, str, array, map with str keys). What a pipeline hands to Cases must be JSON: a `bin`
or a non-str map key in it is a plugin error.

## Messages

Decoy → plugin:

| kind         | fields                                                                    |
| ------------ | ------------------------------------------------------------------------- |
| `hello`      | `protocol: 1`, `decoy: "<version>"`                                       |
| `call`       | `id`, `op` (`input`, `output`, `check`), `input`, `frame?`, `connection?` |
| `hostResult` | `id` (of the plugin's `host`), `ok: true, value` or `ok: false, error`    |
| `close`      | `connection` (an id): drop the state kept for it                          |
| `shutdown`   | (none): exit now                                                          |

Plugin → Decoy:

| kind     | fields                                                                                       |
| -------- | -------------------------------------------------------------------------------------------- |
| `ready`  | `protocol: 1`, `name`, `version`, `directions: [<input or output>]`                          |
| `result` | `id` (of the `call`); `ok: true, value`, or `ok: false, miss`, or `ok: false, error, stack?` |
| `host`   | `id` (the plugin's own counter), `op`, `args: []`                                            |

## Handshake

Decoy writes `hello` first. The worker answers `ready` with the directions it provides. Startup fails if it advertises a different protocol version, exits before readiness, sends no valid `ready` within 10 s, or a Pipeline uses a direction it did not announce.

## Calls

- `call` ids are Decoy's, unique per process. A plugin may answer calls in any order and may run them
  concurrently. Decoy sends a connection's next call only after the previous one answered or timed
  out, so a step that keeps state per connection sees its messages in order unless one timed out.
  After a timeout the late call may still run, even after the connection's `close`.
- `connection` is `{ id, route, session }`, absent for `check`. WebSocket `input` also carries `frame` (`"text"` or `"binary"`).
- `input` runs before Case matching. On WebSocket the first step gets the frame payload as `bin`. Each later step gets the previous step's result. The final result must be suitable for the transport's Message envelope.
- `output` runs on the rendered Behavior payload or a Control API push. Each later step gets the previous step's result. WebSocket sends a final `str` as text or `bin` as binary.
- `check` optionally validates a startup literal for the first worker in the output direction when that worker receives the untransformed value. A worker without a `check` callback answers `ok: true`.
- A `miss` result on `input` records a Case miss for the frame. A `miss` result on `output` is a transform error.
- No `result` within the plugin's `timeout` (default 5 s) is a plugin error; a late `result` is
  ignored.

## Host calls

A plugin sends `host` to use what Decoy loaded; Decoy answers with `hostResult` of the same `id`.

| op                | args            | value                                                |
| ----------------- | --------------- | ---------------------------------------------------- |
| `protobuf.has`    | `[type]`        | `bool`: the full name is a message in Decoy's protos |
| `protobuf.decode` | `[type, bin]`   | the message as JSON, the same mapping gRPC Cases see |
| `protobuf.encode` | `[type, value]` | `bin`; unknown fields are an error with a field path |

During startup checks (before Decoy serves; a reload runs none), `protobuf.encode` only validates: string leaves holding `${` are
skipped, field names and the other leaves are still checked, and the value is an empty `bin`.
