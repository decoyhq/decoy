# TLS BFF (Node) E2E

A small BFF (`src/bff.ts`) answers `GET /orders/{id}` by calling three backends, all served by one
Decoy:

| Backend      | Transport     | Decoy listener                          |
| ------------ | ------------- | --------------------------------------- |
| payments API | https         | `port: 8443` (`tls: auto`)              |
| legacy CRM   | plain http    | `ports.crm: { port: 8092, tls: false }` |
| Inventory    | gRPC over TLS | `port: 8443`                            |

```sh
pnpm --filter ./examples/tls-bff-node test:e2e
pnpm --filter ./examples/tls-bff-node dev    # serve the mocks by hand
```

`tests/run.ts` starts Decoy (or reuses one already serving), then runs the tests with
`NODE_EXTRA_CA_CERTS=mocks/.decoy/ca.pem`, the CA `tls: auto` writes. The BFF itself has no
test-only TLS code: `fetch` and grpc-js `credentials.createSsl()` both use Node's trust store.

| Test (`tests/bff.test.ts`) | Proves                                                                            |
| -------------------------- | --------------------------------------------------------------------------------- |
| https, http and gRPC/TLS   | One order joins a payment (https), a customer (http) and stock (gRPC over TLS).   |
| one Session, all listeners | `useCollection` sent to the http listener switches the gRPC reply on the TLS one. |
| generated cert is trusted  | `https://localhost:8443` verifies against `mocks/.decoy/ca.pem`.                  |

See the [TLS guide](../../docs/guide/tls.md).
