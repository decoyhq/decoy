# Bridge browser WebSockets

Set `webSocketUrls` to route the page's own sockets to Decoy (Playwright 1.48 or later). The page
needs no changes:

```ts title="playwright.config.ts (use excerpt)"
decoyOptions: {
  urls: '**/api/**',
  webSocketUrls: '**/live', // matched against the socket URL, e.g. ws://127.0.0.1:4173/live
}
```

- Each matching page socket is bridged to Decoy: the fixture opens its own socket to Decoy at the
  same path and query (`ws://`, or `wss://` for an `https` `url`) with `x-mock-session` set to the
  test's Session. Text and binary frames pass both ways; when either side closes, the other is
  closed too, with the same code and reason where the protocol allows it. When the
  test ends, the fixture closes its sockets to Decoy that are still open.
- The real server is never contacted. If Decoy can't be reached, the page socket is closed.
- A socket keeps the Session it opened with; see [WebSocket](../../websocket/index.md) for Routes and Behaviors.

To push a message from the test, wait for the page's socket and send to it:

```ts title="tests/live.spec.ts (excerpt)"
test('shows a notice the server pushes', async ({ page, decoy }) => {
  await page.goto('/live.html')

  const live = await decoy.connection('friend-socket', { timeout: 5000 })
  await live.send({ type: 'notice', text: 'hello' })

  await expect(page.getByRole('listitem')).toHaveText('hello')
})
```

- `decoy.connection(route, { timeout })` resolves once this test's Session has a live connection
  on the Route (Control API `wait`). It rejects on timeout (Decoy's default when `timeout` is
  unset), for an unknown Route, or for a Route whose transport has no connections (HTTP, unary gRPC).
- `.send(message)` pushes `message` to every live connection of the Route in this Session, skipping
  Cases (Control API [`send`](../../sessions-and-control/index.md#send)). Other tests' sockets never get it.
- It resolves while _any_ of the Session's sockets on the Route is live, including one that is
  closing. After a reload, wait for a page signal that the new socket is up before you `send`.
