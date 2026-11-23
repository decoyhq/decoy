# Playwright (TypeScript) E2E

A static page calls its API with plain `fetch()`. Playwright tests route those calls to native
Decoy through `@decoyhq/playwright`, so the UI renders mocked data without a backend.

```sh title="Install Chromium once"
pnpm --filter ./examples/playwright-ts-e2e exec playwright install chromium
```

```sh title="Run the Playwright tests"
pnpm --filter ./examples/playwright-ts-e2e test:e2e
```

What happens:

1. Playwright starts the static app (`app/serve.mjs`, port 4173) as its `webServer`.
2. Global setup (`global-setup.ts`) starts a local Passthrough upstream on `127.0.0.1:4174`
   (failing if the port is taken), then Decoy with `cargo run -p decoy -- serve --config
mocks/decoy.yaml` (port 8080), or reuses one already running locally.
3. Each test gets its own Decoy Session on the `happy-path` Collection. Browser requests matching
   `**/api/**` and the `/live` page's WebSocket (`**/live`) go to Decoy; nothing reaches a real
   backend.

| Test                                             | Proves                                                                                                                                                                                                               |
| ------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `users.spec.ts` › loading a user                 | Clicking **Load user** fetches `/api/users/1`, Decoy answers from `mocks/routes/users.yaml`, and the page shows `Ada Lovelace`.                                                                                      |
| `collections.spec.ts` › switching the Collection | `decoy.useCollection('no-users')` before the click makes the same action show `Error 404: user not found` (`mocks/collections.yaml`).                                                                                |
| `overrides.spec.ts` › overriding one route       | `decoy.useRoute('users-by-id', 'user-1', 'grace')` swaps one Behavior (`Grace Hopper`); `decoy.reset()` drops the override (`Ada Lovelace` again).                                                                   |
| `overrides.spec.ts` › unmapped API call          | **Load orders** hits `/api/orders`, which no Route serves. Decoy answers `501` with `x-mock-miss: true`, shown as `Error 501 (Decoy miss): …`; the request never reaches a real backend.                             |
| `passthrough.spec.ts` › Passthrough              | **Load profile** hits `/api/profile`; its Behavior is `kind: passthrough` (`mocks/routes/profile.yaml`), so Decoy forwards it to the local upstream and the page shows the upstream's answer.                        |
| `isolation.spec.ts` › Sessions                   | One test switches its Session to `no-users`; the next test, in a fresh context with a different Session id, still sees `Ada Lovelace`. All spec files also run in parallel (`fullyParallel`) against the same Decoy. |
| `templating.spec.ts` › templated fallback        | **Load second user** hits `/api/users/2`; no Case is specific to it, so the `any` Case's templated Behavior answers `User 2` from `${ pathParams.id }`.                                                              |
| `websocket.spec.ts` › on: connect                | Opening `/live.html` opens a socket bridged to Decoy; the `friend-socket` Route's `on: connect` Case sends `welcome`, shown as `Connected`.                                                                          |
| `websocket.spec.ts` › reply                      | **Ask status** sends `{ type: get-status }`; Decoy replies `Friend: online`.                                                                                                                                         |
| `websocket.spec.ts` › useRoute                   | `decoy.useRoute('friend-socket:status:offline')` on the open socket changes the next reply to `Friend: offline`.                                                                                                     |
| `websocket.spec.ts` › send                       | Two tests meet at a rendezvous on the app server, so they run at once (CI uses 2 workers); each calls `decoy.connection('friend-socket')` and `send`s its own notice, and each page shows only its own.              |
