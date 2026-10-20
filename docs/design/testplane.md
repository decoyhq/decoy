# Testplane support

Status: research prototype (`packages/testplane`, `examples/testplane-ts-e2e`). The example's 8
tests pass locally against real headless Chrome, also with `testplane_sessions_per_browser=3`.

## Goal

Same contract as `@decoyhq/playwright`: Decoy starts once per run, each test gets its own Session,
matching browser requests go to Decoy, misses fail closed. For the user it is one config entry
plus mocks:

```ts
// .testplane.conf.ts
plugins: {
  '@decoyhq/testplane': { urls: '**/api/**', collection: 'happy-path' },
},
```

In tests, `decoy(browser)` returns the Session's Router (`useCollection`, `useRoute`, `reset`,
`sessionId`). No fixtures file, no global-setup file.

## Testplane facts that shape the design

- Testplane 9 runs a WebdriverIO 9 fork (`@testplane/webdriverio`). Sessions are WebDriver Classic
  by default (it adds `wdio:enforceWebDriverClassic`); BiDi only with `webSocketUrl: true`.
- Plugins are `(testplane, opts) => void`, loaded with `require` (CJS) in the master process and
  again in every worker. `testplane.isWorker()` tells them apart.
- Master events include `RUNNER_START` / `RUNNER_END` (async, awaited): the place to start and stop
  Decoy. Workers get `AFTER_TESTS_READ` with a collection whose root suites accept
  `beforeEach` / `afterEach`: the place for per-test Sessions.
- One browser session runs many tests in sequence (`testsPerSession` defaults to Infinity), and
  each test gets a new `browser` object for the same WebDriver session.
- `browser.getPuppeteer()` gives a CDP connection (Chromium) in WebDriver Classic mode.
- Config already has `devServer` (≈ Playwright `webServer`) and master `beforeAll` / `afterAll`.

## Design

| Concern            | Playwright package                       | Testplane prototype                                    |
| ------------------ | ---------------------------------------- | ------------------------------------------------------ |
| Start Decoy        | `globalSetup` file the user adds         | plugin, master `RUNNER_START` / `RUNNER_END`           |
| Per-test Session   | `context` / `decoy` fixtures             | plugin, root `beforeEach` / `afterEach` in workers     |
| Router in tests    | `decoy` fixture                          | `decoy(browser)`                                       |
| Routing            | `context.route` + Node fetch + `fulfill` | CDP request interception, `continue({ url, headers })` |
| Isolation boundary | fresh browser context per test           | Session swapped per test inside a reused browser       |

Routing rewrites the URL inside Chrome (`Fetch.continueRequest` with a new `url`): method, headers,
and body stay as the browser sent them, `x-mock-session` is added, and the page still sees its own
origin (no CORS). Matching requests outside a test are aborted. If Decoy is down the request fails
in the browser; it never goes to the real network.

Gotchas the prototype hit:

- Testplane `require`s the plugin (CJS build) while tests `import { decoy }` (ESM build): two module
  instances. State lives in a `globalThis` registry under `Symbol.for(...)`.
- State is keyed by `browser.sessionId`, not the `browser` object, which changes per test. Keyed
  by object, routing was reinstalled every test and stale handlers aborted requests.

## Open questions

1. **Shared core.** Done: `packages/runner-core` (private) holds start/stop, health, and the Control
   API client. rslib bundles it into each package's code and declarations (api-extractor
   `bundledPackages`), so consumers never install it.
2. **Non-Chromium.** CDP routing is Chromium only. The cross-browser path is BiDi
   `network.addIntercept` + `network.continueRequest({ url, headers })`, used when
   `browser.isBidi`. Check that Firefox honours `url` in `continueRequest`.
3. **Not yet verified end to end:** POST bodies, cookies (does Chrome send the original origin's
   cookies after the rewrite?), requests from new tabs and workers, Selenium grids / cloud
   providers where `getPuppeteer()` can't connect.
4. **Bleed between tests.** The browser is reused, so a request still in flight from test N could
   land in test N+1's Session; it is aborted only if it fires between the hooks. Playwright avoids
   this with a fresh context per test.
5. **Leak.** One registry entry per WebDriver session, never removed. Small, but a worker-side
   session-end hook would clean it.
6. **Shipping.** CI (browser job detection keys on `@playwright/test`), release pack/smoke/publish,
   coverage upload, and user docs (`docs/guide/testplane.md`) are not wired. The package is marked
   `private` until then.
