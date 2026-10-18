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

With [go-task](https://taskfile.dev) installed, `task --list` shows the shortcuts (`task check` runs
what CI gates on). Without it, use the underlying commands:

| Command                                                 | What it does                |
| ------------------------------------------------------- | --------------------------- |
| `cargo test --workspace`                                | Rust unit/integration tests |
| `cargo clippy --workspace --all-targets -- -D warnings` | Rust lint                   |
| `cargo fmt --check`                                     | Rust format check           |
| `pnpm build` / `pnpm typecheck` / `pnpm test`           | JS workspace packages       |
| `pnpm check` / `pnpm fix`                               | ESLint + Prettier           |

Examples (`examples/*`) are the e2e tier and run their own `test:e2e` per workspace.

## Automation

Bots and checks on every PR (Renovate, CodeRabbit, autofix.ci, CodeQL, zizmor, Codecov) and the
settings that enable them: [`docs/repo-automation.md`](docs/repo-automation.md).

## Conventions

- **Conventional Commits**, small **vertical-slice** PRs.
- `@decoyhq/*` is the package scope; the CLI bin is `decoy`.
- **Don't reach for the real network in tests** — Decoy is fail-closed by default; a test that
  leaks to a live upstream is a bug.

## Releasing

One version covers the `decoy` binary and every published npm package. It lives in
`[workspace.package]` in `Cargo.toml`; `pnpm check` fails if a package drifts from it.

```sh
task release -- 0.1.0-alpha.1       # branch release/v…, bump versions, push, open PR
# merge the PR, then:
task release:tag -- 0.1.0-alpha.1   # tag origin/main and push the tag; CI builds and publishes
```

## TypeScript

`tsc` is TypeScript 7 (`@typescript/native` → `typescript@7`). The `typescript` package itself is
the 6.0 API (`@typescript/typescript6`) because typescript-eslint cannot use TypeScript 7 until 7.1
restores the programmatic API. Shortcut: drop the 6.0 alias once typescript-eslint supports 7.1.
