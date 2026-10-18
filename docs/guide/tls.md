# TLS

Some clients only talk HTTPS: a BFF that must call its backends over TLS, an app served over https
in the browser, a gRPC client using TLS credentials. Decoy can serve any listener over TLS, and keep
others plain http next to it.

## Turn it on

```yaml
tls: auto # or: tls: { cert: certs/dev.pem, key: certs/dev-key.pem }
port: 8443 # https
ports:
  payments: 9443 # https
  legacy-crm: { port: 8080, tls: false } # http
```

- `tls` applies to every listener. `{ port, tls: false }` opts one out.
- One TLS port serves HTTP/1.1, HTTP/2, gRPC and the Control API. ALPN picks the protocol per
  connection.
- `tls: auto` signs a fresh localhost cert (valid a year) (`localhost`, `127.0.0.1`, `::1`) at every start
  with a local CA it creates once in `.decoy/` next to the config and reuses on later runs. Decoy
  writes a `.gitignore` there so the CA key stays out of git.
- `tls: { cert, key }` uses your own PEM files instead, e.g. the cert your dev server already uses.

See [Configuration](config.md) for the exact keys.

## Trust the cert

Clients reject a cert they don't trust. Point them at the CA (`tls: auto`) or your cert:

| Client                             | How                                                                                                                                                                                                                                         |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Node (`fetch`, `https`)            | `NODE_EXTRA_CA_CERTS=mocks/.decoy/ca.pem node …` (read at process start)                                                                                                                                                                    |
| `@grpc/grpc-js`                    | `credentials.createSsl()` uses Node's trust, so the same `NODE_EXTRA_CA_CERTS` works                                                                                                                                                        |
| Playwright (`@decoyhq/playwright`) | Run Playwright with `NODE_EXTRA_CA_CERTS=mocks/.decoy/ca.pem`: the fixture calls Decoy from Node. `use: { ignoreHTTPSErrors: true }` only covers the browser and `context.request`. Or point `decoyOptions.url` at a `tls: false` listener. |
| curl                               | `curl --cacert mocks/.decoy/ca.pem https://localhost:8443/…`                                                                                                                                                                                |

Connect to `localhost` (or `127.0.0.1`), the names the generated cert covers.

## Example

[`examples/tls-bff-node`](../../examples/tls-bff-node) is a Node BFF that calls a payments API over
https, a legacy CRM over plain http, and an Inventory gRPC service over TLS, all mocked by one Decoy,
with one Session switching all three.
