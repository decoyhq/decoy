# Playwright

`@decoyhq/playwright` routes your browser's API calls to `decoy`, gives each test its own Session, and
exposes the Control API as a fixture.

```sh
npm i -D @decoyhq/cli @decoyhq/playwright
```

```ts
// playwright.config.ts
import type { DecoyTestOptions } from '@decoyhq/playwright'
import { defineConfig } from '@playwright/test'

export default defineConfig<DecoyTestOptions>({
  globalSetup: './global-setup.ts',
  use: {
    baseURL: 'http://127.0.0.1:4173',
    decoyOptions: {
      command: 'decoy serve --config mocks/decoy.yaml',
      urls: '**/api/**', // browser requests sent to Decoy
      collection: 'happy-path', // each test's Session starts here
    },
  },
})
```

```ts
// global-setup.ts
export { setup as default } from '@decoyhq/playwright'
```

```ts
// tests/fixtures.ts
import { createDecoyFixtures, type DecoyFixtures } from '@decoyhq/playwright'
import { test as base } from '@playwright/test'

export const test = base.extend<DecoyFixtures>(createDecoyFixtures())
export { expect } from '@playwright/test'
```

```ts
// tests/users.spec.ts
import { expect, test } from './fixtures'

test('switching the Collection changes what the page shows', async ({ page, decoy }) => {
  await decoy.useCollection('no-users')

  await page.goto('/')
  await page.getByRole('button', { name: 'Load user' }).click()

  await expect(page.getByRole('status')).toHaveText('Error 404: user not found')
})
```

## Options (`use.decoyOptions`)

| option                | default                       | meaning                                            |
| --------------------- | ----------------------------- | -------------------------------------------------- |
| `url`                 | `http://127.0.0.1:8080`       | where Decoy serves                                 |
| `command`             | `decoy serve`                 | shell command `setup` runs when Decoy isn't up     |
| `cwd`                 | the config file's directory   | working directory for `command`                    |
| `reuseExistingServer` | `true` locally, `false` in CI | reuse a Decoy already answering health             |
| `timeout`             | `60000`                       | ms `setup` waits for health                        |
| `urls`                | none                          | glob/RegExp (or list) of browser requests to route |
| `collection`          | Decoy's default               | Collection each test's Session starts on           |

Decoy is off for projects without `decoyOptions`. `setup` starts one Decoy per distinct `url`:
projects that share a `url` share a Decoy, and each other `url` gets its own `command`. `command`
runs with `node_modules/.bin` (from `cwd` up) on PATH. Teardown sends SIGTERM and, if Decoy hasn't
exited after 8s, SIGKILL (Windows: `taskkill /T /F`). Projects that share a `url` must agree on
`command`, `cwd`, `reuseExistingServer` and `timeout`, or `setup` fails.

## What happens per test

- `setup` (global setup) starts `command` unless Decoy already answers health. Its teardown stops
  only what it started. Fixtures never start Decoy.
- Each test gets a Session id (`<file>-<title>-<uuid>`). Before routing or test code runs, Decoy's
  health is checked and `collection` applied.
- Browser requests matching `urls` go to Decoy with method, path and query, headers, and body kept,
  and `x-mock-session` set. Decoy's answer, including a 501 miss, reaches the page as-is. If Decoy
  is unreachable the request is aborted, never sent to the real network.
- The `decoy` fixture: `sessionId`, `useCollection(id)`, `useRoute(route, case, behavior)`,
  `reset()`, `endSession()`. Each call hits the Control API immediately and rejects with Decoy's
  error. When the test ends, the fixtures call `endSession()` for you, so a long-lived Decoy
  doesn't keep one Session per test.

The runnable version of all this is
[`examples/playwright-ts-e2e`](../../examples/playwright-ts-e2e).
