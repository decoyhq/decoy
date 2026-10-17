# Contributing to Decoy

How to run the repo locally and the rules a change should follow. Vocabulary lives in
[`CONTEXT.md`](CONTEXT.md); decisions in [`docs/adr/`](docs/adr).

## Toolchain

- **Rust** (stable) for `crates/*`
- **pnpm 11** · **Node ≥ 24** for wrappers in `packages/*` and `examples/*`

```sh
pnpm install            # use --frozen-lockfile in CI
```

## Commands

| Command | What it does |
| --- | --- |
| `cargo test --workspace` | Rust unit/integration tests |
| `cargo clippy --workspace --all-targets -- -D warnings` | Rust lint |
| `cargo fmt --check` | Rust format check |
| `pnpm build` / `pnpm typecheck` / `pnpm test` | JS workspace packages |
| `pnpm check` / `pnpm fix` | Biome lint/format |

Examples (`examples/*`) are the e2e tier and run their own `test:e2e` per workspace.

## Conventions

- **Conventional Commits**, small **vertical-slice** PRs.
- `@decoy/*` is the package scope; the CLI bin is `decoy`.
- **Don't reach for the real network in tests** — Decoy is fail-closed by default; a test that
  leaks to a live upstream is a bug.
