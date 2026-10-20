# Testplane

`@decoyhq/testplane` is a [Testplane](https://testplane.io) plugin. It starts `decoy` for the run,
gives each test its own Session, routes the browser's API calls to Decoy, and exposes the Control
API as `decoy(browser)`.

```sh
npm i -D @decoyhq/cli @decoyhq/testplane
```

The setup is one `plugins` entry:

```ts
// .testplane.conf.ts
import type { DecoyOptions } from '@decoyhq/testplane'
import type { ConfigInput } from 'testplane'

export default {
  gridUrl: 'local',
  baseUrl: 'http://127.0.0.1:4173',
  browsers: { chrome: { desiredCapabilities: { browserName: 'chrome' } } },
  plugins: {
    '@decoyhq/testplane': {
      command: 'decoy serve --config mocks/decoy.yaml',
      urls: '**/api/**', // browser requests sent to Decoy
      collection: 'happy-path', // each test's Session starts here
    } satisfies DecoyOptions,
  },
} satisfies ConfigInput
```

```ts
// tests/users.testplane.ts
import { decoy } from '@decoyhq/testplane'
import { it } from 'testplane'

it('switching the Collection changes what the page shows', async ({ browser }) => {
  await decoy(browser).useCollection('no-users')

  await browser.url('/')
  await browser.$('#load-user').click()

  await expect(browser.$('#result')).toHaveText('Error 404: user not found')
})
```

## Options

| option                | default                       | meaning                                                   |
| --------------------- | ----------------------------- | --------------------------------------------------------- |
| `enabled`             | `true`                        | `false` turns the plugin off without removing it          |
| `url`                 | `http://127.0.0.1:8080`       | where Decoy serves                                        |
| `command`             | `decoy serve`                 | shell command the plugin runs when Decoy isn't up         |
| `cwd`                 | `process.cwd()`               | working directory for `command`                           |
| `reuseExistingServer` | `true` locally, `false` in CI | reuse a Decoy already answering health                    |
| `timeout`             | `60000`                       | ms the plugin waits for health                            |
| `urls`                | none                          | glob (`*` in a segment, `**` across) or RegExp, or a list |
| `collection`          | Decoy's default               | Collection each test's Session starts on                  |

`command` runs with `node_modules/.bin` (from `cwd` up) on PATH. At the end of the run the plugin
sends SIGTERM and, if Decoy hasn't exited after 8s, SIGKILL (Windows: `taskkill /T /F`).

## What happens per test

- Before tests run, the plugin starts `command` unless Decoy already answers health. When the
  run ends it stops only what it started.
- Each test gets a Session id (`<browser>-<full title>-<uuid>`). Before the test, Decoy's health is
  checked and `collection` applied. After it, the Session ends.
- Browser requests matching `urls` go to Decoy with method, path and query,
  headers, cookies, and body kept, and `x-mock-session` set. Chrome rewrites the URL itself, so
  the page sees its own origin (no CORS). Decoy's answer, including a 501 miss, reaches the page
  as-is. If Decoy is unreachable the request fails, never sent to the real network. Matching
  requests fired between tests are aborted.
- `decoy(browser)`: `sessionId`, `useCollection(id)`, `useRoute(route, case, behavior)`,
  `reset()`, `endSession()`. Each call hits the Control API immediately and rejects with Decoy's
  error. It throws outside a test.

## Limits

- Routing (`urls`) needs Chrome or Chromium: it uses CDP through `browser.getPuppeteer()`, in
  WebDriver Classic and BiDi (`webSocketUrl: true`) sessions alike. On other browsers each test
  fails with a clear error.
- Testplane reuses a browser for many tests, so a request still in flight when a test ends can
  reach the next test's Session. Wait for your page to settle before a test ends.

The runnable version of all this is
[`examples/testplane-ts-e2e`](../../examples/testplane-ts-e2e).
