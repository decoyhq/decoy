# @decoy/playwright

Playwright fixtures and global setup for the native `decoy` runtime.

```ts
// playwright.config.ts
export default defineConfig<DecoyTestOptions>({
  globalSetup: './global-setup.ts', // export { setup as default } from '@decoy/playwright'
  use: {
    decoyOptions: { url: 'http://127.0.0.1:8080', command: 'decoy serve' },
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

Browser routing and Control methods (`useCollection`, `useRoute`, `reset`) land in #181–#183.
