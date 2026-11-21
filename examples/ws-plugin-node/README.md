# WebSocket plugins (Node) E2E

Two binary socket formats, each mocked through a plugin step. Laid out like a user's repo: the
plugins are packages (`acme/`, standing in for npm packages), configured in the user's own plugin
files (`plugins/`), which `mocks/decoy.yaml` runs. The HTTP Route `POST /approvals` shares
the typed-envelope Pipeline with the WebSocket Route at `/ws/approvals`.

```sh
pnpm --filter ./examples/ws-plugin-node test:e2e
pnpm --filter ./examples/ws-plugin-node dev    # serve the mocks on 127.0.0.1:8095 by hand
```

The plugins import `@decoyhq/plugin` from its build, so run `pnpm build` once first. The test
starts its own Decoy process on an ephemeral loopback port, using `DECOY_BINARY_PATH` when set
and `cargo run -p decoy` otherwise.

```
acme/decoy-plugin-typed-envelope/   msgpack { type, data: <protobuf> } envelopes, JSON text frames
acme/decoy-plugin-binary-rpc/       [kind][topic][id][protobuf payload] frames
plugins/typed-envelope.ts           the user's file: export default typedEnvelope()
plugins/binary-rpc.ts               the user's file: binaryRpc({ topics: { … } })
plugins/binary-rpc.test.ts          unit tests of the steps, no Decoy
mocks/decoy.yaml                    transformers and named pipelines
proto/                              compiled by Decoy; plugins use it through ctx.protobuf
```

| Test (`tests/sockets.test.ts`) | Proves                                                                                  |
| ------------------------------ | --------------------------------------------------------------------------------------- |
| typed envelopes                | One socket mixes JSON text (`{ action: ping }` on connect) and binary protobuf replies. |
| send through the plugin        | Control API `send` is encoded by the plugin; a failed output skips delivery.            |
| a frame the plugin misses      | `miss()` skips the frame and logs why; the socket keeps working.                        |
| rpc REQUEST → REPLY            | The reply echoes the request's `id` with `${ message.id }`.                             |
| another Session                | Same path, `broken` Collection: an ERROR frame instead of the REPLY.                    |
| events numbered per connection | Plugin `ctx.state` is per socket: each one counts its events from 1.                    |
| an unknown kind                | A miss; the next request is still answered.                                             |

Try breaking it: a typo in a payload (`nmae: Ann` in `mocks/routes/rpc.yaml`) fails startup through
the plugin's `check`; a `throw` in a plugin's `input` shows up as an error event with the frame's
bytes and the plugin's stack.

See the [WebSocket guide](../../docs/guide/websocket.md#transformers).

[Transformer benchmark notes](transformer-benchmarks.md) compare worker and command execution.
