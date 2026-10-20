# Testplane (TypeScript) E2E

The same static page and mocks as `playwright-ts-e2e`, driven by [Testplane](https://testplane.io).
The only Decoy wiring is one `plugins` entry in `.testplane.conf.ts`.

```sh
pnpm --filter ./examples/testplane-ts-e2e test:e2e   # downloads Chrome for Testing on first run
```

What happens:

1. Testplane starts the static app (`app/serve.mjs`, port 4173) as its `devServer`.
2. The config's `beforeAll` starts a local Passthrough upstream on `127.0.0.1:4174`.
3. `@decoyhq/testplane` starts Decoy with `cargo run -p decoy -- serve --config mocks/decoy.yaml`
   (port 8080), or reuses one already running locally.
4. Every test runs twice: in `chrome` (WebDriver Classic, Testplane's default) and `chrome-bidi`
   (`webSocketUrl: true`).
5. Each test gets its own Decoy Session on the `happy-path` Collection. Browser requests matching
   `**/api/**` go to Decoy; nothing reaches a real backend.

| Test                                              | Proves                                                                                                        |
| ------------------------------------------------- | ------------------------------------------------------------------------------------------------------------- |
| `users.testplane.ts` › loading a user             | **Load user** fetches `/api/users/1`; Decoy answers `Ada Lovelace`.                                           |
| `collections.testplane.ts` › switching Collection | `decoy(browser).useCollection('no-users')` makes the same click show `Error 404: user not found`.             |
| `overrides.testplane.ts` › overriding one route   | `useRoute('users-by-id', 'user-1', 'grace')` shows `Grace Hopper`; `reset()` drops it.                        |
| `overrides.testplane.ts` › unmapped API call      | `/api/orders` has no Route: Decoy's fail-closed `501` miss reaches the page.                                  |
| `passthrough.testplane.ts` › Passthrough          | `/api/profile` is forwarded by Decoy to the local upstream.                                                   |
| `echo.testplane.ts` › body, cookies, headers      | **Send echo** POSTs JSON with a cookie and `x-client`; Decoy's template echoes all three (`Ada / abc / app`). |
| `isolation.testplane.ts` › Sessions               | Two tests in the same browser session get different Sessions; a switched Collection doesn't leak.             |
| `templating.testplane.ts` › templated fallback    | `/api/users/2` is answered by the `any` Case's template: `User 2`.                                            |
