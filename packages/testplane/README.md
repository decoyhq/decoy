# @decoyhq/testplane

[Testplane](https://testplane.io) plugin for the native `decoy` runtime. Research prototype: not
published yet.

```ts
// .testplane.conf.ts
export default {
  plugins: {
    '@decoyhq/testplane': {
      url: 'http://127.0.0.1:8080',
      command: 'decoy serve',
      urls: '**/api/**', // browser requests sent to Decoy
      collection: 'happy-path', // each test's Session starts here
    },
  },
} satisfies ConfigInput

// some.testplane.ts
import { decoy } from '@decoyhq/testplane'

it('empty state', async ({ browser }) => {
  await decoy(browser).useCollection('empty')
  await decoy(browser).useRoute('users-by-id', 'user-123', 'missing')
  await decoy(browser).reset()
})
```

- The master process starts `command` on `RUNNER_START` unless Decoy already answers
  `GET /__decoy__/health` at `url` (reuse allowed locally, refused when `CI` is set), and stops it
  on `RUNNER_END` only if it started it.
- Each test gets its own Session: a root `beforeEach` checks Decoy's health, applies `collection`,
  and a root `afterEach` ends the Session. `decoy(browser)` is that Session's Router.
- With `urls`, matching requests from every page of the test's browser go to Decoy with
  `x-mock-session` set. Chrome rewrites the request URL itself (CDP `Fetch.continueRequest`), so
  method, headers, and body are kept and the page sees its own origin. If Decoy is unreachable the
  request fails; it never reaches the real network. Requests between tests are aborted.
- Chromium only for now (CDP through `browser.getPuppeteer()`).
