# @decoyhq/playwright

Playwright fixtures and global setup for the native `decoy` runtime.

```sh
npm i -D @decoyhq/cli @decoyhq/playwright
```

`@decoyhq/cli` installs the `decoy` binary into `node_modules/.bin`, and `setup` runs `command` with
every `node_modules/.bin` up from `cwd` on PATH, so the default `command: 'decoy serve'` works as is,
from an npm script, the VS Code extension, or `npx playwright test`.

```ts
// playwright.config.ts
export default defineConfig<DecoyTestOptions>({
  globalSetup: './global-setup.ts', // export { setup as default } from '@decoyhq/playwright'
  use: {
    decoyOptions: {
      url: 'http://127.0.0.1:8080',
      command: 'decoy serve',
      urls: '**/api/**', // browser requests sent to Decoy
      webSocketUrls: '**/live', // page WebSockets bridged to Decoy
      collection: 'happy-path', // each test's Session starts here
    },
  },
})

// some.spec.ts
import { expect, test } from '@decoyhq/playwright'

test('empty state', async ({ page, decoy }) => {
  await decoy.useCollection('empty')
  await decoy.useRoute('users-by-id:user-123:missing') // or ('users-by-id', 'user-123', 'missing')
  await decoy.reset()
})

test('server push', async ({ page, decoy }) => {
  await page.goto('/live.html')
  const live = await decoy.connection('friend-socket', { timeout: 5000 }) // the page's socket
  await live.send({ type: 'notice', text: 'hello' })
})
```

`test` is Playwright's `test` with the Decoy fixtures merged in, and `expect` is Playwright's. To
combine them with your own fixtures, `test.extend` it, or merge `createDecoyFixtures()` into your
base yourself: `base.extend<DecoyFixtures>(createDecoyFixtures())`.

- `setup` starts `command` unless Decoy already answers `GET /__decoy__/health` at `url`.
  Reuse follows Playwright `webServer`: allowed locally, refused when `CI` is set. The returned
  teardown stops Decoy only if `setup` started it. A Decoy that `setup` started in an earlier run
  that never tore down (a hard-killed runner) is stopped and started fresh, not reused.
- Fixtures never start Decoy. Each test gets its own Session: Decoy's health is checked and
  `collection` applied once, before routed requests or test code run.
- The `decoy` fixture is this Session's Router: `sessionId`, `useCollection`, `useRoute`,
  `reset`, `endSession`. `useRoute` takes a Behavior address, either as one `'route:case:behavior'`
  string or as three arguments; a malformed address rejects without calling Decoy. Each call hits the Control API immediately and rejects with Decoy's error,
  or after 10s without an answer.
  It fails if `decoyOptions` is unset. When the test ends, the fixtures end its Session in Decoy.
- Decoy is off unless `decoyOptions` is set.
- With `urls`, every test's browser context sends matching requests to Decoy, with no need to
  request the `decoy` fixture. Method, path/query, headers, and body are kept; `x-mock-session` is
  set to the test's Session. Decoy's response (including a fail-closed miss) goes back to the
  browser as-is (Set-Cookie included, for the page's origin; forwarding keeps no cookies of its
  own); if Decoy is unreachable the request is aborted, never sent to the real network.
  Other requests continue normally.
- With `webSocketUrls` (Playwright 1.48+), every matching page WebSocket is bridged to Decoy at the
  same path and query, with `x-mock-session` set on the upgrade. Frames pass both ways and a close
  on either side closes the other. The real server is never contacted; if Decoy is unreachable the
  page socket is closed.
- `decoy.connection(routeId, { timeout })` waits until this test's Session has a live connection on
  the Route; its `.send(message)` pushes to that Session's connections only. Call `connection`
  again after a reconnect.
- Only the fixture's `context` (and its `page`) is routed. A context you create yourself with
  `browser.newContext()` is not: its requests skip Decoy and carry no Session.
