# Plugin wire protocol, version 1

Status: **spec** for the plugin host in `decoy-runtime` and the `@decoyhq/plugin` SDK. What plugins
are for and how users configure them: `docs/design/transports.md` §7 and ADR 0004. This note is for
someone writing an SDK in another language.

## Process

- Decoy starts one process per `plugins:` entry with `command` run by the shell, in the directory of
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

| kind         | fields                                                                             |
| ------------ | ---------------------------------------------------------------------------------- |
| `hello`      | `protocol: 1`, `decoy: "<version>"`                                                |
| `call`       | `id`, `op` (`decode`, `encode`, `check`), `step`, `input`, `frame?`, `connection?` |
| `hostResult` | `id` (of the plugin's `host`), `ok: true, value` or `ok: false, error`             |
| `close`      | `connection` (an id): drop the state kept for it                                   |
| `shutdown`   | (none): exit now                                                                   |

Plugin → Decoy:

| kind     | fields                                                                                       |
| -------- | -------------------------------------------------------------------------------------------- |
| `ready`  | `protocol: 1`, `name`, `version`, `steps: [<name>]`, `functions: []`                         |
| `result` | `id` (of the `call`); `ok: true, value`, or `ok: false, miss`, or `ok: false, error, stack?` |
| `host`   | `id` (the plugin's own counter), `op`, `args: []`                                            |

## Handshake

Decoy writes `hello` first. The plugin answers `ready` with the step names it serves. Startup fails
when the plugin answers another `protocol`, exits first, sends no valid `ready` within 10 s, or a
pipeline names a step it didn't list. v1 has no op that calls functions; `functions` is reserved and must be `[]`.

## Calls

- `call` ids are Decoy's, unique per process. A plugin may answer calls in any order and may run them
  concurrently. Decoy sends a connection's next call only after the previous one answered or timed
  out, so a step that keeps state per connection sees its messages in order unless one timed out.
  After a timeout the late call may still run, even after the connection's `close`.
- `connection` is `{ id, route, session }`, absent for `check`. `frame` is `"text"` or `"binary"` on
  `decode`, absent otherwise.
- `decode`: inbound. The first step gets the frame's payload as `bin` (text frames as their UTF-8
  bytes); each later step gets the value the previous one returned.
- `encode`: outbound, steps in reverse. The step nearest the Cases gets the rendered message; each
  step outward gets the value the inner one returned. The outermost step returns a `str` (sent as a
  text frame) or `bin` (a binary frame).
- `check`: at startup, once per outbound literal, on the step nearest the Cases. `input` is the
  message as written, `${ }` templates included. A step without `check` answers `ok: true`.
- `miss` (a reason) is only meaningful from `decode`: the frame is a MISS. From `encode`, it counts
  as an error.
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
