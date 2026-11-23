# Runnable examples

Each project includes its own mocks and a test that drives a real Decoy server.

| Example                                                          | What to try                                                          |
| ---------------------------------------------------------------- | -------------------------------------------------------------------- |
| [Playwright](playwright-ts-e2e/README.md)                        | Switch the response a page sees in a browser test.                   |
| [Testplane](testplane-ts-e2e/README.md)                          | Route browser requests to a per-test Session.                        |
| [gRPC with Node](grpc-node/README.md)                            | Return a protobuf message or a gRPC status error.                    |
| [WebSocket with Node](ws-node/README.md)                         | Reply to a message or push one to an open socket.                    |
| [HTTP and WebSocket Transformers](ws-transformer-node/README.md) | Use workers and commands to mock binary HTTP and WebSocket payloads. |
| [TLS with Node](tls-bff-node/README.md)                          | Call HTTPS, HTTP, and gRPC backends from one app.                    |

Follow the [repository setup](../CONTRIBUTING.md#toolchain) and build the workspace packages with
`pnpm build`. Each example's README has the command for that project. For browser prerequisites
and the command to run the whole suite, see [Contributing](../CONTRIBUTING.md#commands).

## Conventions every example follows

- A workspace member with its own `mocks/` — nothing shared between examples.
- A `package.json` exposing `test:e2e` and, where a human can poke it, `dev`.
- A short `README.md`: the command to run and which feature each step proves.
