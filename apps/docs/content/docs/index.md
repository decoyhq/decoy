---
title: Decoy documentation
description: Serve a mock locally, switch responses in a running app, and isolate parallel tests.
---

Start with [Serve your first mock](quick-start/index.mdx). You'll make one request return a user, then a 404. The quick start includes installation, both YAML files, and the commands to check each response. If you only need the CLI, [choose an installation method](install/index.mdx).

> Cloudflare preview smoke test (#472): this marker should appear only in the preview.

## Write mocks

| I want to…                                                   | Read                                                                                  |
| ------------------------------------------------------------ | ------------------------------------------------------------------------------------- |
| Match a path, inspect the request body, or delay a reply     | [HTTP Routes, Cases, and Behaviors](routes/index.md)                                  |
| Switch several responses together                            | [Collections](collections/index.md)                                                   |
| Return a protobuf message or stream replies                  | [gRPC](grpc/index.md)                                                                 |
| Respond to a WebSocket message or close a connection         | [WebSocket](websocket/index.md)                                                       |
| Decode a custom payload before matching it or encode a reply | [Transformers](transformers/index.md) and [Pipelines](transformer-pipelines/index.md) |

## Drive Decoy from a test

Use the [Playwright fixture](playwright/index.mdx) or [Testplane plugin](testplane/index.mdx) to route browser requests to Decoy. Each test gets a Session, so one test can select an error while another still sees a successful response.

For curl or another HTTP client, use the [Control API](sessions-and-control/index.md). For a complete setup with its own mocks, browse the [runnable examples](examples/index.md).

## Run the server

- [Configuration](config/index.md) lists YAML keys, CLI flags, and defaults.
- [TLS](tls/index.md) shows how to serve HTTPS with a local certificate.
- [Terminal UI](tui/index.md) explains how to inspect requests and change the active Collection.
- [Troubleshooting](troubleshooting/index.md) helps you tell a mock miss from a connection or Session problem.
- [Glossary](terms/index.md) defines the terms used in mock files and the Control API.
