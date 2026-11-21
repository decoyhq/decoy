# WebSocket Transformers (Node) E2E

Two binary socket formats, each mocked through a Transformer. Laid out like a user's repo: the
workers are packages (`acme/`, standing in for npm packages), configured in the user's own worker
files (`plugins/`), which `mocks/decoy.yaml` runs.

```sh
pnpm --filter ./examples/ws-plugin-node test:e2e
pnpm --filter ./examples/ws-plugin-node dev    # serve the mocks on 127.0.0.1:8095 by hand
```

The workers import `@decoyhq/plugin` from its build, so run `pnpm build` once first. The test
starts its own Decoy process on an ephemeral loopback port, using `DECOY_BINARY_PATH` when set
and `cargo run -p decoy` otherwise.

```
acme/decoy-plugin-typed-envelope/   msgpack { type, data: <protobuf> } envelopes, JSON text frames
acme/decoy-plugin-binary-rpc/       [kind][topic][id][protobuf payload] frames
plugins/typed-envelope.ts           the user's file: export default typedEnvelope()
plugins/binary-rpc.ts               the user's file: binaryRpc({ topics: { … } })
plugins/binary-rpc.test.ts          unit tests of the steps, no Decoy
mocks/decoy.yaml                    transformers and named pipelines
proto/                              compiled by Decoy; workers use it through ctx.protobuf
```

| Test (`tests/sockets.test.ts`) | Proves                                                                                  |
| ------------------------------ | --------------------------------------------------------------------------------------- |
| typed envelopes                | One socket mixes JSON text (`{ action: ping }` on connect) and binary protobuf replies. |
| send through the worker        | Control API `send` is encoded by the worker; a failed output skips delivery.            |
| a frame the worker misses      | `miss()` skips the frame and logs why; the socket keeps working.                        |
| rpc REQUEST → REPLY            | The reply echoes the request's `id` with `${ message.id }`.                             |
| another Session                | Same path, `broken` Collection: an ERROR frame instead of the REPLY.                    |
| events numbered per connection | Worker `ctx.state` is per socket: each one counts its events from 1.                    |
| an unknown kind                | A miss; the next request is still answered.                                             |

Try breaking it: a typo in a payload (`nmae: Ann` in `mocks/routes/rpc.yaml`) fails startup through
the worker's `check`; a `throw` in a worker's `input` shows up as an error event with the frame's
bytes and the worker's stack.

See the [WebSocket guide](../../apps/docs/content/guide/websocket.md#transformers).

[Transformer benchmark notes](transformer-benchmarks.md) compare worker and command execution.
