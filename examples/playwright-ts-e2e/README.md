# Playwright (TypeScript) E2E

A static page calls its API with plain `fetch()`. Playwright tests route those calls to native
Decoy through `@decoy/playwright`, so the UI renders mocked data without a backend.

```sh
pnpm --filter ./examples/playwright-ts-e2e exec playwright install chromium   # once
pnpm --filter ./examples/playwright-ts-e2e test:e2e
```

What happens:

1. Playwright starts the static app (`app/serve.mjs`, port 4173) as its `webServer`.
2. Global setup (`global-setup.ts`) starts Decoy with `cargo run -p decoy-runtime -- serve --config
   mocks/decoy.yaml` (port 8080), or reuses one already running locally.
3. Each test gets its own Decoy Session on the `happy-path` Collection. Browser requests matching
   `**/api/**` go to Decoy; nothing reaches a real backend.

| Test | Proves |
| --- | --- |
| `users.spec.ts` › loading a user | Clicking **Load user** fetches `/api/users/1`, Decoy answers from `mocks/routes/users.yaml`, and the page shows `Ada Lovelace`. |
| `collections.spec.ts` › switching the Collection | `decoy.useCollection('no-users')` before the click makes the same action show `Error 404: user not found` (`mocks/collections.yaml`). |
