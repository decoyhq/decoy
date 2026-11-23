# HTTP and WebSocket Transformers (Node) E2E

Two binary socket formats, each mocked through a Transformer step. The `acme/` packages export
Transformer factories. The user's worker files (`workers/`) call those factories, and
`mocks/decoy.yaml` runs the workers. The HTTP Route `POST /approvals` shares
the typed-envelope Pipeline with the WebSocket Route at `/ws/approvals`. `POST /compressed-approvals`
and `/ws/compressed-approvals` also share a Pipeline: `gzip` decompresses incoming bytes before
`env` decodes them for Case matching. The reply goes through `env` and then `gzip`. The HTTP
Route declares `Content-Encoding: gzip` so the client knows to decompress it.
[`POST /http-context`](mocks/routes/http-context.yaml) shows a worker selecting a Case based on a
`preview=1` cookie and using the original request metadata again on output.

```sh title="Run the Transformer tests"
pnpm --filter ./examples/ws-transformer-node test:e2e
```

To serve the mocks by hand on `127.0.0.1:8095`:

```sh title="Start the Transformer example server"
pnpm --filter ./examples/ws-transformer-node dev
```

The workers import `@decoyhq/transformer` from its build, so run `pnpm build` once first. The test
starts its own Decoy process on an ephemeral loopback port, using `DECOY_BINARY_PATH` when set
and `cargo run -p decoy` otherwise.

```text title="Example files and directories"
acme/decoy-transformer-typed-envelope/  msgpack { type, data: <protobuf> } envelopes, JSON text frames
acme/decoy-transformer-binary-rpc/      [kind][topic][id][protobuf payload] frames

workers/typed-envelope.ts              the user's file: export default typedEnvelope()
workers/binary-rpc.ts                   the user's file: binaryRpc({ topics: { … } })
workers/binary-rpc.test.ts              unit tests of the steps, no Decoy

mocks/decoy.yaml                        Transformers and named Pipelines
proto/                                 compiled by Decoy; workers use it through ctx.protobuf
```

| Test (`tests/sockets.test.ts`)      | Proves                                                                                  |
| ----------------------------------- | --------------------------------------------------------------------------------------- |
| typed envelopes                     | One socket mixes JSON text (`{ action: ping }` on connect) and binary protobuf replies. |
| shared HTTP and WebSocket Pipelines | Both transports use the same typed envelopes, with and without gzip.                    |
| send through the worker             | Control API `send` is encoded by the worker; a failed output skips delivery.            |
| a frame the worker misses           | `miss()` skips the frame and logs why; the socket keeps working.                        |
| rpc REQUEST → REPLY                 | The reply echoes the request's `id` with `${ message.id }`.                             |
| another Session                     | Same path, `broken` Collection: an ERROR frame instead of the REPLY.                    |
| events numbered per connection      | Worker `ctx.state` is per socket: each one counts its events from 1.                    |
| an unknown kind                     | A miss; the next request is still answered.                                             |

Try breaking it: a typo in a payload (`nmae: Ann` in `mocks/routes/rpc.yaml`) fails startup through
the worker's `check`; a `throw` in a worker's `input` shows up as an error event with the frame's
bytes and the worker's stack.

See [Configure a Transformer Pipeline](../../apps/docs/content/guide/transformer-pipelines.md) for shared setup.

[Transformer benchmark notes](transformer-benchmarks.md) compare worker and command execution.
