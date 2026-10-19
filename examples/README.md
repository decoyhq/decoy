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

`test:e2e` starts Decoy with `cargo run -p decoy`; set `DECOY_BINARY_PATH` to the absolute path of
a prebuilt `decoy` (e.g. `"$PWD/target/debug/decoy"` from the repo root) to run that instead. CI runs one job per example, with
`DECOY_BINARY_PATH` set to a binary built once; browser examples install Chromium in their own job.
