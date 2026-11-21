![Decoy, a portable local mock server for development and testing](apps/docs/public/brand/decoy-banner.svg)

# Decoy

Decoy is a local mock server for HTTP, gRPC, and WebSocket clients. Define your Routes,
point your app at Decoy, and switch the same request from success to failure while the app is
running. When a request misses, Decoy returns a diagnostic. Forwarding to an upstream requires
an explicit Passthrough Behavior.

## Try it

1. [Install Decoy](apps/docs/content/guide/install.md) as a standalone binary or a dev dependency in your JS project.
2. [Serve your first mock](apps/docs/content/quickstart.mdx). Make `GET /users/123` return Ada, then switch it to a 404.
3. [Open the terminal UI](apps/docs/content/guide/tui.md) to inspect requests and switch Collections interactively.

![Decoy's terminal UI showing the request log, Collection switcher, and request details](docs/assets/tui.gif)

## Use it in tests

Parallel tests can share one Decoy server. Each test gets its own Session, so switching a Collection
in one test leaves the others on their own scenarios. The [Playwright fixture](apps/docs/content/guide/playwright.md)
and [Testplane plugin](apps/docs/content/guide/testplane.md) manage Sessions for you. Other clients can set the
Session header and call the [Control API](apps/docs/content/guide/sessions-and-control.md).

Browse the [user guide](apps/docs/content/index.md) for configuration and transport details, or start
with a [runnable example](examples/README.md).

## Contributing

For local builds and checks, see [Contributing](CONTRIBUTING.md). The [documentation index](docs/README.md) links to the glossary, design records, and maintainer runbook.
