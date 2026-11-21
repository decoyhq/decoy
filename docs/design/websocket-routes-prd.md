# WebSocket Routes PRD

## Problem Statement

When my app uses a WebSocket, I want to test both messages it sends and messages the server pushes. I need Decoy to match the connection and individual messages, choose Behaviors from the test's Session, and send a deterministic sequence. I also need the same setup to work with the browser's own WebSocket in Playwright.

A Behavior can send one message or a list. I want the list to identify each item's payload and make it clear which settings apply to the entire Behavior and which apply to a single item.

## Solution

Add WebSocket Routes to Decoy. Match the upgrade to a Route, then run an optional `on: connect` Case and match incoming messages against the Route's other Cases. Behaviors can send a single `message`, a timed list of `messages` whose items contain `payload`, or a close frame. Test code can wait for a connection and push messages through the Control API. Playwright can bridge selected page sockets to Decoy in the test's Session.

## User Stories

1. As a Route author, I want WebSocket Routes on Decoy's usual port, so that I can mock HTTP and WebSocket traffic with one runtime.
2. As a Route author, I want to match the upgrade path and query, so that I can distinguish services sharing a socket URL.
3. As a Route author, I want ambiguous upgrade matches identified during config loading, so that Route selection is predictable.
4. As a Route author, I want a Case to run when the socket opens, so that I can mock a server that sends first.
5. As a Route author, I want Cases to match JSON fields and plain text, so that I can mock both structured messages and simple commands.
6. As a Behavior author, I want to template replies from the incoming message and upgrade metadata, so that I can echo IDs and use captured path values.
7. As a Behavior author, I want a single `message` shorthand, so that I can write simple replies with one field.
8. As a Behavior author, I want `messages` items to use `payload`, so that the item's content is distinct from its settings.
9. As a Behavior author, I want an initial Behavior delay and optional item delays, so that I can model a pause and subsequent timed updates.
10. As a Behavior author, I want invalid settings rejected at load time, so that a typo does not silently change a Scenario.
11. As a Behavior author, I want an empty list of messages, so that the connection can stay open until the test sends a message.
12. As a Behavior author, I want to close the socket with a code and reason, so that I can exercise retry and error handling.
13. As a tester, I want a Case miss logged while the socket stays open, so that I can diagnose a message and continue the test.
14. As a tester, I want to change a Route's active Behavior while its socket is open, so that I can change the Scenario without reconnecting.
15. As a tester, I want connections scoped to a Session, so that parallel tests can use the same Route independently.
16. As a tester, I want to wait for a connection and push a message to it, so that I can trigger a server-side update at a precise point in the test.
17. As a Playwright user, I want to select which page sockets go to Decoy, so that I can use the app's existing WebSocket client.
18. As a Playwright user, I want the bridge to propagate closure and clean up sockets when the test ends, so that connections do not leak into later tests.

## Implementation Decisions

- Serve WebSocket upgrades on the usual Decoy listener, including `wss://` when TLS is configured. A WebSocket Route matches the upgrade path and optional query values. Prefer the match with more query constraints. Detect Route definitions that can tie and resolve upgrades without a unique WebSocket Route through HTTP handling.
- Allow one `on: connect` Case per Route. Run it on open using upgrade metadata. Match subsequent text frames against the selected Cases using the Message envelope. Parse valid JSON text into a value and expose other text as a string. Log unmatched messages as misses and keep the connection open. The baseline WebSocket transport treats inbound binary frames as misses.
- A Behavior that sends data uses one `message` or a `messages` list. A close-only Behavior can omit both. List items require `payload` and may have `delay`. Reject the old item key `message` at config load. Keep the top-level `message` shorthand. Render templates from the Message envelope, send strings as text, and serialize structured values as JSON text.
- A Behavior-level `delay` waits once before its first action, including when the Behavior has a list. An item-level `delay` applies before that item. The first item waits for both delays when both are present. Later item delays are relative to the prior send. Both levels accept nonnegative integer milliseconds or duration strings with `ms` or `s`. Invalid values fail Route loading.
- Preserve the order of a Behavior's messages. Separate Behavior runs can interleave on independent timers. A configured `close` follows that Behavior's messages. Closing the socket cancels its pending sends.
- Bind each connection to the Session from its upgrade. Evaluate its current Selection for each inbound message. Reconnects run the open Case with the Session's current Selection.
- Expose connected-only `wait` and `send` through the Control API. Both scope connections by Route and Session. `send` bypasses Cases and reports how many connections accepted the message. Use the configured timeout when no connection can accept the operation.
- Let Playwright bridge page sockets matched by `webSocketUrls`. Preserve the path and query, forward messages and close information, and attach the test Session to the Decoy-side upgrade. Close bridged connections when the test ends.

## Testing Decisions

- Load Routes through the public config seam. Verify `messages` with `payload`, rejection of the old item key and malformed delays, and acceptance of the single `message` shorthand. Cover both delay scopes together.
- Launch Decoy and use real WebSocket clients to assert observable frames, order, and close behavior. Extend the existing Node WebSocket tests for open Cases, matching, misses, Control API pushes, and independent Sessions. Verify that the initial wait and the first item delay combine and that later item delays apply only to their own messages.
- Exercise the Playwright bridge through a browser and the fixture. Check that the page receives a reply in the correct Session and observes closure through its ordinary WebSocket API.

## Out of Scope

- Transformations, binary payload support in the runtime, the SDK worker protocol, and SSE.
- Renaming `message` in the inbound Message envelope or the Control API.
- Repeat timers and new Control API verbs or wait conditions.

## Further Notes

The Node WebSocket examples and Playwright fixture tests provide the reference test seams. Document the route schema, both delay scopes, Sessions, and test-driven pushes in the WebSocket guide.

Implementation is tracked in [#416](https://github.com/decoyhq/decoy/issues/416).
