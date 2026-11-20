# WebSocket transformer recipes

Run these examples with Decoy and plain commands. This server config requires no plugin worker or SDK build.

| Route                                       | What you configure                                                                                                      | What the client receives                                                |
| ------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------- |
| [One command](routes/approval-command.yaml) | One argv list per direction. The command reads a JSON topic-to-schema map.                                              | Approval objects for both watch and lookup requests on the same socket. |
| [Two commands](routes/approval-chain.yaml)  | A framing step and a protobuf step in each direction. They pass a MessagePack record with topic and bytes between them. | The same protocol as the one-command Route.                             |
| [Native command](routes/user-native.yaml)   | One Rust binary per direction. The outbound argv selects the `user` topic.                                              | The same lookup request yields a User object on this Route.             |

Mocks use domain fields. For example, the native Route can reply with:

```yaml
message: { id: a-1, name: Ada }
```

The [schema map](schema-map.json) maps incoming `meta.topic` to the right protobuf schema. The configured outbound topic determines both the reply schema and the topic stamped onto the wire envelope.

From the repository root with port 8095 free:

```sh
pnpm install --frozen-lockfile --ignore-scripts --package-import-method=copy
cargo build --locked -p decoy
cargo build --locked -p decoy-runtime --example ws_transformer
DECOY_BINARY_PATH="$PWD/target/debug/decoy" node examples/ws-plugin-node/transformer-usage/demo.mjs
```

The demo uses real sockets and verifies every reply. The command paths are relative to the directory containing decoy.yaml. Each Route selects a pipeline by name. See the [runtime comparison](../transformer-prototype/README.md) for measurements and remaining design choices.

## Persistent worker without the SDK

The [worker example](raw-worker.mjs) speaks [protocol v1](../../../docs/design/plugin-protocol.md) over stdin/stdout using only a MessagePack library and the example codec. It sends protobuf requests back to the Decoy host while handling the original call. This is the path for a community SDK that wants persistence, host calls, or connection state.

With the same prerequisites and port 8095 free:

```sh
DECOY_BINARY_PATH="$PWD/target/debug/decoy" node examples/ws-plugin-node/transformer-usage/demo.mjs worker
```
