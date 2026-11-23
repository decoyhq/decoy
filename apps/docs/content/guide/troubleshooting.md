# Troubleshoot a mock

Start with what your client saw: no connection, a mock miss, or a valid response you didn't expect.

## The client can't connect

Check that Decoy is listening at the URL your client uses. For a local server on the default port:

```sh title="Check whether Decoy is listening"
curl -i http://localhost:8080/__decoy__/health
```

Look for `200 OK` and `{"status":"ok"}`. If you can't connect, check the address and port in Decoy's startup log. If [TLS](tls.md) is enabled, use `https://` and trust the configured certificate or local CA. A healthy server can still miss a mock: the health check doesn't match a Route or read a Session.

## Decoy returned a mock miss

For HTTP, `501` with `x-mock-miss: true` means no active Case matched. Read `candidates` in the response: it lists the attempted Behavior addresses and where they failed to match. For example, `field: pathParams.id` points to a Case condition on the URL parameter. Check that the intended Behavior is in the active [Collection](collections.md), then compare the actual request with the [Route and Case](routes.md#case-match).

Run Decoy in the [terminal UI](tui.md) to inspect recent requests with `Ctrl+O`. For gRPC, see [misses and errors](grpc.md#misses-and-errors).

## Switching Collections didn't change the response

Check that the switch and the request use the same [Session](sessions-and-control.md#sessions). A curl request without `x-mock-session` uses the default Session. Playwright and Testplane use a separate Session for each test; switch through the adapter before the page makes the request.

If the Session is right, check whether a later Case in the Collection matches the request or a Behavior override is active. In the terminal UI, `/status` shows the selected Collection and overrides for the active Session. If you just edited a file, check that [reload](config.md#reloading) succeeded: Decoy keeps serving the previous configuration on a validation error.
