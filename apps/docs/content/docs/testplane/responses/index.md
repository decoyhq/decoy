# Switch responses in a test

Use `decoy(browser)` to select a Collection before the page makes its request. This test selects `no-users` and checks the error that the page renders.

```ts title="tests/users.testplane.ts"
import { decoy } from '@decoyhq/testplane'
import { it } from 'testplane'

it('switching the Collection changes what the page shows', async ({ browser }) => {
  await decoy(browser).useCollection('no-users')

  await browser.url('/')
  await browser.$('#load-user').click()

  await expect(browser.$('#result')).toHaveText('Error 404: user not found')
})
```

## Session lifecycle

- Before tests run, the plugin starts `command` unless Decoy already answers health. When the
  run ends it stops only what it started.
- Each test gets a Session id (`<browser>-<full title>-<uuid>`). Before the test and its hooks
  (including a file's top-level `beforeEach`), Decoy's health is checked and `collection` applied.
  After the test and its `afterEach` hooks, the Session ends.
- Browser requests matching `urls`, from every tab, popup, cross-site iframe, and worker (dedicated,
  shared, service), go to Decoy with method, path and query, headers, cookies, and body kept, and
  `x-mock-session` set. Chrome rewrites the URL itself, so the page sees its own origin (no CORS).
  Decoy's answer, including a 501 miss, reaches the page as-is; a redirect's relative `Location`
  resolves against the page's URL, not Decoy's. If Decoy is unreachable the request
  fails, never sent to the real network. New tabs stay paused until routing is on, so even their
  first request is covered. Matching requests fired between tests are aborted.
- `decoy(browser)`: `sessionId`, `useCollection(id)`, `useRoute(address)` with a Behavior address
  `route:case:behavior` (or `useRoute(route, case, behavior)`), `reset()`, `endSession()`. Each call
  hits the Control API immediately and rejects with Decoy's error. It throws outside a test.
