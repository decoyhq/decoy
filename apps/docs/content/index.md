---
title: Decoy documentation
description: Serve a mock locally, switch responses in a running app, and isolate parallel tests.
---

Start with [Serve your first mock](quickstart.mdx). You'll make one request return a user, then a 404. The quick start includes installation, both YAML files, and the commands to check each response. If you only need the CLI, [choose an installation method](guide/install.md).

## Write mocks

| I want to…                                                   | Read                                                                                  |
| ------------------------------------------------------------ | ------------------------------------------------------------------------------------- |
| Match a path, inspect the request body, or delay a reply     | [HTTP Routes, Cases, and Behaviors](guide/routes.md)                                  |
| Switch several responses together                            | [Collections](guide/collections.md)                                                   |
| Return a protobuf message or stream replies                  | [gRPC](guide/grpc.md)                                                                 |
| Respond to a WebSocket message or close a connection         | [WebSocket](guide/websocket.md)                                                       |
| Decode a custom payload before matching it or encode a reply | [Transformers](guide/transformers.md) and [Pipelines](guide/transformer-pipelines.md) |

## Drive Decoy from a test

Use the [Playwright fixture](guide/playwright.md) or [Testplane plugin](guide/testplane.md) to route browser requests to Decoy. Each test gets a Session, so one test can select an error while another still sees a successful response.

For curl or another HTTP client, use the [Control API](guide/sessions-and-control.md). For a complete setup with its own mocks, browse the [runnable examples](../../../examples/README.md).

## Run the server

- [Configuration](guide/config.md) lists YAML keys, CLI flags, and defaults.
- [TLS](guide/tls.md) shows how to serve HTTPS with a local certificate.
- [Terminal UI](guide/tui.md) explains how to inspect requests and change the active Collection.
- [Troubleshooting](guide/troubleshooting.md) helps you tell a mock miss from a connection or Session problem.
- [Glossary](terms.md) defines the terms used in mock files and the Control API.
