# @decoy/playwright

Playwright fixtures and global setup for the native `decoy` runtime.

```ts
// playwright.config.ts
export default defineConfig<DecoyTestOptions>({
  globalSetup: './global-setup.ts', // export { setup as default } from '@decoy/playwright'
  use: {
    decoyOptions: {
      url: 'http://127.0.0.1:8080',
      command: 'decoy serve',
      urls: '**/api/**', // browser requests sent to Decoy
    },
  },
})

// fixtures.ts
export const test = base.extend<DecoyFixtures>(createDecoyFixtures())
```

- `setup` starts `command` unless Decoy already answers `GET /__decoy__/health` at `url`.
  Reuse follows Playwright `webServer`: allowed locally, refused when `CI` is set. The returned
  teardown stops Decoy only if `setup` started it.
- Fixtures never start Decoy. The `decoy` fixture fails if `decoyOptions` is unset or Decoy is
  unreachable, and exposes this test's `sessionId`.
- Decoy is off unless `decoyOptions` is set.
- With `urls`, every test's browser context sends matching requests to Decoy, with no need to
  request the `decoy` fixture. Method, path/query, headers, and body are kept; `x-mock-session` is
  set to the test's Session. Decoy's response (including a fail-closed miss) goes back to the
  browser as-is; if Decoy is unreachable the request is aborted, never sent to the real network.
  Other requests continue normally.

Control methods (`useCollection`, `useRoute`, `reset`) land in #182–#183.
