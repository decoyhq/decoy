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
      collection: 'happy-path', // each test's Session starts here
    },
  },
})

// fixtures.ts
export const test = base.extend<DecoyFixtures>(createDecoyFixtures())

// some.spec.ts
test('empty state', async ({ page, decoy }) => {
  await decoy.useCollection('empty')
  await decoy.useRoute('users-by-id', 'user-123', 'missing')
  await decoy.reset()
})
```

- `setup` starts `command` unless Decoy already answers `GET /__decoy__/health` at `url`.
  Reuse follows Playwright `webServer`: allowed locally, refused when `CI` is set. The returned
  teardown stops Decoy only if `setup` started it.
- Fixtures never start Decoy. Each test gets its own Session: Decoy's health is checked and
  `collection` applied once, before routed requests or test code run.
- The `decoy` fixture is this Session's Router: `sessionId`, `useCollection`, `useRoute`,
  `reset`. Each call hits the Control API immediately and rejects with Decoy's error. It fails if
  `decoyOptions` is unset.
- Decoy is off unless `decoyOptions` is set.
- With `urls`, every test's browser context sends matching requests to Decoy, with no need to
  request the `decoy` fixture. Method, path/query, headers, and body are kept; `x-mock-session` is
  set to the test's Session. Decoy's response (including a fail-closed miss) goes back to the
  browser as-is; if Decoy is unreachable the request is aborted, never sent to the real network.
  Other requests continue normally.
