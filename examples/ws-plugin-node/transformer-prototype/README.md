# WebSocket transformer prototype

This throwaway example compares command pipelines with the worker stack in PRs #402–#406. All six Routes run through Decoy. On each binary Route, `watch` and `lookup` select different protobuf schemas from the **same socket**. The config maps the actual wire field `meta.topic` to a schema. Cases receive the decoded JSON and Behaviors declare plain messages. The outbound command takes a fixed topic as an argv parameter for this example.

```text
client → MessagePack { meta: { topic }, payload: <gRPC-framed protobuf> }
       → select schema from topic → protobuf JSON → Decoy Case

Decoy Behavior message → protobuf with configured schema → gRPC framing
                       → MessagePack with configured topic → client
```

The gRPC layer here is the uncompressed five-byte message prefix. The transport is WebSocket.

## Run

From the repository root:

```sh
pnpm install --frozen-lockfile --ignore-scripts --package-import-method=copy
pnpm --filter @decoyhq/plugin build
cargo build --locked -p decoy
cargo build --locked -p decoy-runtime --example ws_transformer
DECOY_BINARY_PATH="$PWD/target/debug/decoy" node examples/ws-plugin-node/transformer-prototype/bench.mjs
```

Run with port 8095 free. The harness alternates two message types, verifies the full reply on each round trip, warms up every socket, rotates mode order and reports p50, p95, p99 and requests per second. Set `ROUNDS=1` for a quick verification. The commands resolve relative to the example directory where Decoy starts. See [command Route](../mocks/routes/transformer-prototype/command.yaml) and [split Route](../mocks/routes/transformer-prototype/split.yaml).

| Route      | Round trip                                                                                                                    |
| ---------- | ----------------------------------------------------------------------------------------------------------------------------- |
| `text`     | Decoy's native JSON text path                                                                                                 |
| `sdk`      | Persistent SDK worker running the same Node codec as the commands                                                             |
| `sdk-host` | Persistent SDK worker using Decoy's `ctx.protobuf` host calls                                                                 |
| `command`  | Decoy launches one Node process per direction                                                                                 |
| `native`   | Decoy launches one Rust process per direction, compiling the example protos on each invocation                                |
| `split`    | Decoy launches two Node processes per direction. The intermediate MessagePack record carries both the topic and payload bytes |

The benchmark compares small messages on one host. Per-command figures include Node startup, JS module loading and, for the protobuf step, schema loading. The Rust command gives a native reference. Generated code or compiled descriptor sets could change these figures again. `healthMs` is smoke-test output for the whole server, including startup polling, and is not per-mode startup data. For spawn-only measurements, compare a no-op payload command and the real command with Hyperfine separately.

## Measured on the Decoy runtime

macOS arm64, 12 available cores, debug Rust builds and Node 24.22.1 invoked directly. Three rounds with 72 measured requests per mode for one socket and 192 for eight sockets. Recorded on the workspace filesystem. Compare the modes within this run.

| Mode       | 1 socket p50 / p95 (ms) | 1 socket req/s | 8 sockets p50 / p95 (ms) | 8 sockets req/s |
| ---------- | ----------------------: | -------------: | -----------------------: | --------------: |
| `text`     |             0.16 / 0.27 |           5546 |              0.29 / 0.90 |           19622 |
| `sdk`      |             0.46 / 0.99 |           1647 |              0.78 / 1.76 |            8151 |
| `sdk-host` |             0.44 / 1.29 |           1441 |              1.12 / 1.84 |            7070 |
| `command`  |         117.76 / 155.81 |              8 |          191.11 / 250.23 |              39 |
| `split`    |         231.10 / 301.27 |              4 |          362.69 / 416.61 |              21 |
| `native`   |           12.54 / 37.24 |             62 |            14.87 / 22.23 |             494 |

## Sustained runs

The 64-socket soak completed **3,241,728 verified round trips** across the text baseline and two persistent modes. Each mode completed 1,080,576 calls. The harness rotated mode order across three rounds.

| Mode       | Sockets |     Calls | p50 (ms) | p95 (ms) | p99 (ms) | Calls/s |
| ---------- | ------: | --------: | -------: | -------: | -------: | ------: |
| `text`     |      64 | 1,080,576 |     0.47 |     0.99 |     1.66 | 111,422 |
| `sdk`      |      64 | 1,080,576 |     1.17 |     1.78 |     2.61 |  52,017 |
| `sdk-host` |      64 | 1,080,576 |     2.46 |     3.13 |     5.01 |  24,971 |

Run with `PROFILE=soak ROUNDS=3` and the same command above.

The command profile ran separately at eight sockets, with 1,024 calls per mode. These calls include one inbound and one outbound transform per round trip. The two-process variant spawns four processes per round trip.

| Mode      | Sockets | Calls | p50 (ms) | p95 (ms) | p99 (ms) | Calls/s |
| --------- | ------: | ----: | -------: | -------: | -------: | ------: |
| `command` |       8 | 1,024 |   170.18 |   237.39 |   322.20 |      44 |
| `split`   |       8 | 1,024 |   345.49 |   405.48 |   466.61 |      23 |
| `native`  |       8 | 1,024 |    13.43 |    16.42 |    28.94 |     572 |

Run with `PROFILE=commands ROUNDS=1`. Socket count differs between profiles. Use the equal-concurrency comparison above when comparing persistence and commands directly. The results come from local macOS arm64 debug builds and small payloads. [Raw run output](measurements.json).

## Draft proposal for the active PRs

- [#402 design](https://github.com/decoyhq/decoy/pull/402). Define independent inbound and outbound transformer lists, each running top to bottom. Put static invocation args in argv arrays. The application command owns the mapping from wire discriminators to schemas. For different outbound schemas, use a discriminator the application already exposes or add a named outbound transform to the Behavior alongside `message`. Implement that selector once a representative case needs it.
- [#403 SDK](https://github.com/decoyhq/decoy/pull/403) and [#404 host](https://github.com/decoyhq/decoy/pull/404). Keep the persistent worker available while reviewing the measurements. Decide separately whether to publish the SDK or document a language-neutral worker contract. Binary RPC's per-socket counter and protobuf host services need an explicit home if worker support goes away. One-shot processes provide neither automatically.
- [#405 runtime](https://github.com/decoyhq/decoy/pull/405). Keep the existing WS pipeline seam around Case resolution and Behavior rendering. Commands take bytes on stdin and return bytes on stdout. Inbound must end in JSON, outbound starts with JSON. The prototype uses binary replies, launch-directory paths and a two-second timeout per command. Before shipping, settle path resolution, output bounds, error versus miss, mixed text/binary handling, and whether startup checking and Control API `send` should invoke commands. The current runtime dry-runs encoding for Control API `send`, then encodes again when each receiver sends the frame.
- [#406 examples](https://github.com/decoyhq/decoy/pull/406). Use the opcode/topic-based example to demonstrate several schemas per Route without protobuf names in mock messages. Keep binary RPC as the example for request IDs and per-socket state.

Keep both execution modes in this prototype so the comparison stays reproducible. Use the measurements to decide whether one-shot commands should be the default, an option for simple mocks, or just a local debugging tool.
