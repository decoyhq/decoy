# WebSocket (Node) E2E

Node's built-in `WebSocket` opens two sockets on one path, told apart by the `service` query param,
the way a page talks to two backend services. Decoy answers from `mocks/`.

```sh
pnpm --filter ./examples/ws-node test:e2e
pnpm --filter ./examples/ws-node dev    # serve the mocks on 127.0.0.1:8094 by hand
```

The test starts Decoy with `cargo run -p decoy -- serve --config mocks/decoy.yaml` (port 8094), or
reuses one already serving there.

| Test (`tests/sockets.test.ts`) | Proves                                                                                                         |
| ------------------------------ | -------------------------------------------------------------------------------------------------------------- |
| on: connect, picked by query   | `match.query` picks `friend-socket` or `best-friend-socket` on one path; each `on: connect` Case sends `ping`. |
| message Cases                  | `match: { message: { type: get-status } }` answers with `${ message.id }`; an unmatched message gets nothing.  |
| timed `messages`               | `subscribe` sends `subscribed`, then `update` 200ms later.                                                     |
| manual-start + `send`          | The `silent` Behaviors keep the sockets quiet; Control API `send` pushes `ping` to one Route in one Session.   |
| useRoute on an open socket     | A Selection change applies to the socket's next message.                                                       |
| close while the page sends     | Decoy reads until the page's close reply, so a busy page still gets `4002`, not `1006`.                        |
| `close`                        | The `unauthorized` Behavior sends its error, then closes with `4001`.                                          |
| unmatched upgrade              | A `service` no Route lists is refused with the HTTP miss.                                                      |

See the [WebSocket guide](../../docs/guide/websocket.md).
