# Decoy guide

[Install Decoy](install.md), then follow the [HTTP quickstart](quickstart.md) to serve a response
and switch it to a 404 without restarting the server.

## Write mocks

- [HTTP Routes, Cases, and Behaviors](routes.md) covers request matching, response templates, delays, and Passthrough.
- [Collections](collections.md) explains how to select Behaviors for a scenario and which Case wins when several match.
- [gRPC](grpc.md) covers protobuf schemas, unary replies, errors, and server streaming.
- [WebSocket](websocket.md) covers connections, incoming messages, timed replies, and closing a socket. Use [Transformers](websocket.md#transformers) for custom payload formats.

## Run and inspect Decoy

- [Configuration](config.md) lists CLI flags, config file keys, listener settings, and reload options.
- [TLS](tls.md) explains how to serve HTTPS and trust the local certificate.
- [Terminal UI](tui.md) shows how to inspect a request and change the active Collection.

## Drive Decoy from tests

- [Sessions and the Control API](sessions-and-control.md) explains how to isolate tests and change responses over HTTP.
- [Playwright](playwright.md) and [Testplane](testplane.md) show how to route browser requests and switch Behaviors in a test.
- [Runnable examples](../../examples/README.md) include complete test projects with their own mocks.

See the [glossary](../../CONTEXT.md) for definitions of Decoy terms.
