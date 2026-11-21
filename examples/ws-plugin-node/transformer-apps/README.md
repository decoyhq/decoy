# WebSocket transformer examples

[decoy.yaml](decoy.yaml) is where you configure the worker, commands, and named pipelines. A Route references one pipeline and declares the messages the application sees.

- **Expense approvals dashboard.** A browser subscribes to an expense and sees it as pending. When a manager approves it, the test pushes the update to the same WebSocket through Decoy's Control API. On the wire, MessagePack meta.topic selects the protobuf schema. The persistent worker implements the wire protocol directly and uses Decoy for protobuf calls. The test sends only the expense ID and the new state.
- **Freezer monitoring.** A legacy telemetry gateway sends gzip-compressed JSON over a binary WebSocket. The client subscribes to a sensor and receives a temperature snapshot. The pipeline uses the system gzip CLI directly. The mock uses the ordinary JSON payload.

These are runnable examples of application flows. Their wire formats are fixtures defined in this repository. Run from the repository root with gzip installed and port 8095 free:

```sh
pnpm install --frozen-lockfile --ignore-scripts --package-import-method=copy
cargo build --locked -p decoy
DECOY_BINARY_PATH="$PWD/target/debug/decoy" node examples/ws-plugin-node/transformer-apps/demo.mjs
```

The demo checks decoded responses over real sockets and confirms that an invalid gzip message is skipped. See the [benchmark](../transformer-benchmarks.md) for execution trade-offs and [protocol v1](../../../docs/design/plugin-protocol.md) for implementing a persistent worker in another language.
