# gRPC (Node) E2E

A plain `@grpc/grpc-js` client calls `approvals.v1.Approvals/GetApproval`. Decoy answers from
`mocks/` using the proto in `proto/`. There's no real Approvals service and no generated code.

```sh
pnpm --filter ./examples/grpc-node test:e2e
pnpm --filter ./examples/grpc-node dev    # serve the mocks on 127.0.0.1:8091 by hand
```

The test starts Decoy with `cargo run -p decoy -- serve --config mocks/decoy.yaml` (port
8091), or reuses one already serving there.

The Route selects `pipeline: approval-ids` in [`mocks/decoy.yaml`](mocks/decoy.yaml):

```yaml
pipelines:
  approval-ids:
    input: [grpc-frame, protobuf, approval-id]
    output: [approval-id, protobuf, grpc-frame]
```

The built-ins decode the request before the [Node command](transformers/approval-id.ts)
lowercases its ID. `A-1` matches the `pending` Case as `a-1`. On output, the command prefixes the
rendered ID with `ref-` before the built-ins encode and frame the reply.

| Test (`tests/approvals.test.ts`) | Proves                                                                                                                                       |
| -------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| input and output Pipeline        | `A-1` becomes `a-1` before matching the `pending` Case. The client receives `ref-a-1`.                                                       |
| templates                        | `${ message.id }` and `${ metadata."x-user" }` render into the `echo` reply.                                                                 |
| useCollection → status error     | A Session on `approvals-down` gets `NOT_FOUND`, the templated status message, and an `x-reason` trailer. Other Sessions keep the happy path. |
| useRoute                         | One Session overrides `approvals-get:pending` to `approved`.                                                                                 |
| unmapped call                    | A method no Route serves fails closed with `UNIMPLEMENTED` and `x-mock-miss: true`.                                                          |

See the [gRPC guide](../../docs/guide/grpc.md).
