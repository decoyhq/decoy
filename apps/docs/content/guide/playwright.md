# Playwright

`@decoyhq/playwright` routes your browser's API calls to `decoy`, gives each test its own Session, and
exposes the Control API as a fixture.

```sh
npm i -D @decoyhq/cli @decoyhq/playwright
```

```ts title="playwright.config.ts"
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

```ts title="global-setup.ts"
export { setup as default } from '@decoyhq/playwright'
```

```ts title="tests/fixtures.ts"
import { createDecoyFixtures, type DecoyFixtures } from '@decoyhq/playwright'
import { test as base } from '@playwright/test'

export const test = base.extend<DecoyFixtures>(createDecoyFixtures())
export { expect } from '@playwright/test'
```

Next, [switch responses in a test](playwright/responses.md). For a page that opens a socket, [bridge browser WebSockets](playwright/websockets.md).

## Options (`use.decoyOptions`)

| option                | default                       | meaning                                            |
| --------------------- | ----------------------------- | -------------------------------------------------- |
| `url`                 | `http://127.0.0.1:8080`       | where Decoy serves                                 |
| `command`             | `decoy serve`                 | shell command `setup` runs when Decoy isn't up     |
| `cwd`                 | the config file's directory   | working directory for `command`                    |
| `reuseExistingServer` | `true` locally, `false` in CI | reuse a Decoy already answering health             |
| `timeout`             | `60000`                       | ms `setup` waits for health                        |
| `urls`                | none                          | glob/RegExp (or list) of browser requests to route |
| `webSocketUrls`       | none                          | glob/RegExp (or list) of page WebSockets to bridge |
| `collection`          | Decoy's default               | Collection each test's Session starts on           |

Decoy is off for projects without `decoyOptions`. `setup` starts one Decoy per distinct `url`:
projects that share a `url` share a Decoy, and each other `url` gets its own `command`. `command`
runs with `node_modules/.bin` (from `cwd` up) on PATH. Teardown sends SIGTERM and, if Decoy hasn't
exited after 8s, SIGKILL (Windows: `taskkill /T /F`). Projects that share a `url` must agree on
`command`, `cwd`, `reuseExistingServer` and `timeout`, or `setup` fails.

The runnable version of all this is
[`examples/playwright-ts-e2e`](../../../../examples/playwright-ts-e2e).
