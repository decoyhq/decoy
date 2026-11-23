# Switch responses in a test

Use the `decoy` fixture to select a Collection before the page makes its request. This test selects `no-users` and checks the error that the page renders.

```ts title="tests/users.spec.ts"
import { expect, test } from '@decoyhq/playwright'

test('switching the Collection changes what the page shows', async ({ page, decoy }) => {
  await decoy.useCollection('no-users')

  await page.goto('/')
  await page.getByRole('button', { name: 'Load user' }).click()

  await expect(page.getByRole('status')).toHaveText('Error 404: user not found')
})
```

## Session lifecycle

- `setup` (global setup) starts `command` unless Decoy already answers health. Its teardown stops
  only what it started. Fixtures never start Decoy.
- Each test gets a Session id (`<file>-<title>-<uuid>`). Before routing or test code runs, Decoy's
  health is checked and `collection` applied.
- Browser requests matching `urls` go to Decoy with method, path and query, headers, and body kept,
  and `x-mock-session` set. Decoy's answer, including a 501 miss, reaches the page as-is. If Decoy
  is unreachable the request is aborted, never sent to the real network.
- The `decoy` fixture: `sessionId`, `useCollection(id)`, `useRoute(address)` with a Behavior address
  `route:case:behavior` (or `useRoute(route, case, behavior)`), `reset()`, `endSession()`, and
  `connection(route)` (see [WebSockets](../websockets/index.md)). Each call
  hits the Control API immediately and rejects with Decoy's error. When the test ends, the fixtures
  call `endSession()` for you, so a long-lived Decoy doesn't keep one Session per test.
