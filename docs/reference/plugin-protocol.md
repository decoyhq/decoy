# Transformer worker protocol, version 1

Use this specification to write a Transformer worker in another language. It describes the
messages exchanged by Decoy and the [TypeScript SDK](../../packages/plugin/README.md).
To configure a worker for a Route, see [Configure a Transformer Pipeline](../../apps/docs/content/guide/transformer-pipelines.md).

## Process

- Decoy starts one process per `transformers` entry with a `worker` argv list, in the directory of
  `decoy.yaml`, with `DECOY_PLUGIN=1` added to the environment.
- stdin and stdout carry the protocol and nothing else: anything else on stdout is a protocol error
  that ends the worker, so an SDK sends console output to stderr. stderr is free text: Decoy forwards each line
  as `[<key>: <name>@<version>] <line>` and keeps the last 20 lines for exit reports.
- The process lives as long as Decoy. Decoy sends `shutdown` when it stops and kills the process if it
  is still running 2 s later. If the process exits on its own, Decoy reports the exit code and stderr
  tail and exits non-zero. Decoy never restarts a worker, not even on a `--watch` reload.

## Framing

Each message is a 4-byte big-endian length, then that many bytes of one msgpack map. A message is at
most 16 MiB; a longer length ends the worker as a protocol error. Decoy never sends a longer one: a
`call` that wouldn't fit fails as a worker error for that frame only. Every map has a string `kind`. MessagePack carries these messages between Decoy and the worker. The application payload inside a message can use its own format.

## Values

Values are msgpack. Wire bytes are msgpack `bin`; everything else is the JSON data model (nil, bool,
integer, float, str, array, map with str keys). What a pipeline hands to Cases must be JSON: a `bin`
or a non-str map key in it is a worker error.

## Messages

Decoy → worker:

| kind         | fields                                                                    |
| ------------ | ------------------------------------------------------------------------- |
| `hello`      | `protocol: 1`, `decoy: "<version>"`                                       |
| `call`       | `id`, `op` (`input`, `output`, `check`), `input`, `frame?`, `connection?` |
| `hostResult` | `id` (of the worker's `host`), `ok: true, value` or `ok: false, error`    |
| `close`      | `connection` (an id): drop the state kept for it                          |
| `shutdown`   | (none): exit now                                                          |

Worker → Decoy:

| kind     | fields                                                                                       |
| -------- | -------------------------------------------------------------------------------------------- |
| `ready`  | `protocol: 1`, `name`, `version`, `directions: [<input or output>]`                          |
| `result` | `id` (of the `call`); `ok: true, value`, or `ok: false, miss`, or `ok: false, error, stack?` |
| `host`   | `id` (the worker's own counter), `op`, `args: []`                                            |

## Handshake

Decoy writes `hello` first. The worker answers `ready` with the directions it provides. Startup fails if it advertises a different protocol version, exits before readiness, sends no valid `ready` within 10 s, or a Pipeline uses a direction it did not announce.

## Example exchange

Suppose a WebSocket Route uses a worker to turn a JSON object into a text frame. The table shows
the decoded MessagePack maps in the order they cross the worker's pipes. Each row is one message.

| Sender | Message                                                                                                                  |
| ------ | ------------------------------------------------------------------------------------------------------------------------ |
| Decoy  | `{"kind":"hello","protocol":1,"decoy":"0.1.0"}`                                                                          |
| Worker | `{"kind":"ready","protocol":1,"name":"json-text","version":"1.0.0","directions":["output"]}`                             |
| Decoy  | `{"kind":"call","id":7,"op":"output","input":{"status":"ok"},"connection":{"id":"c1","route":"updates","session":"s1"}}` |
| Worker | `{"kind":"result","id":7,"ok":true,"value":"{\"status\":\"ok\"}"}`                                                       |

The worker copies the call's `id` into its reply. Its `value` is a string, so when this is the
last output step, the WebSocket client receives the text frame `{"status":"ok"}`.

## Calls

- `call` ids are Decoy's, unique per process. A worker may answer calls in any order and may run them
  concurrently. Decoy sends a connection's next call only after the previous one answered or timed
  out, so a step that keeps state per connection sees its messages in order unless one timed out.
  After a timeout the late call may still run, even after the connection's `close`.
- `connection` is `{ id, route, session }`, absent for `check`. WebSocket `input` also carries `frame` (`"text"` or `"binary"`).
- `input` runs before Case matching. On WebSocket the first step gets the frame payload as `bin`. Each later step gets the previous step's result. The final result must be suitable for the transport's Message envelope.
- `output` runs on the rendered Behavior payload or a Control API push. Each later step gets the previous step's result. WebSocket sends a final `str` as text or `bin` as binary.
- `check` optionally validates a startup literal for the first worker in the output direction when that worker receives the untransformed value. A worker without a `check` callback answers `ok: true`.
- A `miss` result on `input` records a Case miss for the frame. A `miss` result on `output` is a transform error.
- Set the worker's `timeout` in `ms` or `s`. The default is `5s`. Decoy reports a worker error when the timeout expires and ignores any later `result`.

## Host calls

A worker sends `host` to use what Decoy loaded; Decoy answers with `hostResult` of the same `id`.

| op                | args            | value                                                |
| ----------------- | --------------- | ---------------------------------------------------- |
| `protobuf.has`    | `[type]`        | `bool`: the full name is a message in Decoy's protos |
| `protobuf.decode` | `[type, bin]`   | the message as JSON, the same mapping gRPC Cases see |
| `protobuf.encode` | `[type, value]` | `bin`; unknown fields are an error with a field path |

During startup checks (before Decoy serves; a reload runs none), `protobuf.encode` only validates: string leaves holding `${` are
skipped, field names and the other leaves are still checked, and the value is an empty `bin`.
