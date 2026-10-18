# gRPC (Node) E2E

A plain `@grpc/grpc-js` client calls `approvals.v1.Approvals/GetApproval`. Decoy answers from
`mocks/` using the proto in `proto/`. There's no real Approvals service and no generated code.

```sh
pnpm --filter ./examples/grpc-node test:e2e
pnpm --filter ./examples/grpc-node dev    # serve the mocks on 127.0.0.1:8091 by hand
```

The test starts Decoy with `cargo run -p decoy -- serve --config mocks/decoy.yaml` (port
8091), or reuses one already serving there.

| Test (`tests/approvals.test.ts`) | Proves                                                                                                                                       |
| -------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| a Case matches the request       | `match: { message: { id: a-1 } }` selects the `pending` Case; the reply is the literal `ok` message.                                         |
| templates                        | `${ message.id }` and `${ metadata."x-user" }` render into the `echo` reply.                                                                 |
| useCollection → status error     | A Session on `approvals-down` gets `NOT_FOUND`, the templated status message, and an `x-reason` trailer. Other Sessions keep the happy path. |
| useRoute                         | One Session overrides `approvals-get:pending` to `approved`.                                                                                 |
| unmapped call                    | A method no Route serves fails closed with `UNIMPLEMENTED` and `x-mock-miss: true`.                                                          |

See the [gRPC guide](../../docs/guide/grpc.md).
