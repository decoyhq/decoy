# examples/

End-to-end **user flows**: each example is a self-contained project that drives a real `decoy`
runtime through a real client. They are the e2e tier and expose **`test:e2e` only** — the fast
inner loop (`pnpm test`) never boots an example or a browser.

## Conventions every example follows

- A workspace member with its own `mocks/` — nothing shared between examples.
- A `package.json` exposing `test:e2e` and, where a human can poke it, `dev`.
- A short `README.md`: the command to run and which feature each step proves.

## Running

```sh
pnpm test:e2e                          # every example's test:e2e
pnpm --filter ./examples/<name> dev    # boot one example by hand
```

CI runs one job per example; browser examples install Chromium in their own job.
