# Contributing to Decoy

How to run the repo locally and the rules a change should follow. Vocabulary lives in
[`CONTEXT.md`](CONTEXT.md); decisions in [`docs/adr/`](docs/adr).

## Toolchain

- **Rust** (stable) for `crates/*`
- **pnpm 12** · **Node ≥ 24** for wrappers in `packages/*` and `examples/*`

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

Run `task bench:rust` to measure HTTP hits, gRPC replies, live connection fan-out, and TUI log
frames. The benchmarks use in-process fixtures and report median release timings over five batches.
Compare on an idle machine with the same Rust toolchain. For the original baseline (`8edafdf`),
copy the two runtime bench files and their `[[bench]]` manifest entries to that checkout, and use
`register` in place of `register_shared` in the queue benchmark. For the TUI baseline, copy just
the `log_frame_timing` test into the same test module. The two Cargo commands in `task bench:rust`
then exercise the same workloads on both revisions.

Examples (`examples/*`) are the e2e tier and run their own `test:e2e` per workspace.

`task e2e` builds the packages and the `decoy` binary, then runs every example's `test:e2e`.
Examples boot Decoy with `cargo run` against the built `@decoyhq/*` packages, so the Rust
toolchain must be installed. Set `DECOY_BINARY_PATH` to the absolute path of a prebuilt `decoy`
(e.g. `"$PWD/target/debug/decoy"`) to skip the examples' `cargo run`, as CI does; `task e2e` still
builds it with cargo. Browser examples also need Chromium once (`pnpm install` skips the download):

```sh
pnpm --filter ./examples/playwright-ts-e2e exec playwright install --with-deps chromium
```

## Where documentation goes

Publish a product requirements document (PRD) as a GitHub issue. Choose **Issues → New issue →
Feature PRD**, fill in the prompts, and link the issue from the implementation PR. If you use
another editor, copy the body of the [PRD template](.github/ISSUE_TEMPLATE/feature-prd.md) into an issue.
For a completed example, see the [WebSocket Routes PRD](https://github.com/decoyhq/decoy/issues/416).

Write an ADR once a choice is accepted if reversing it would be costly, there was a real
alternative, and a future contributor would need the reasoning to understand the choice. Most
features need only an issue and a guide update. For instance, the [WebSocket PRD](https://github.com/decoyhq/decoy/issues/416)
describes what a test author needs. [ADR 0004](docs/adr/0004-built-in-steps-only-for-grpc.md)
explains why reversing the input steps on output would fail for some clients.

For a new ADR, take the next four-digit number in [the index](docs/adr/README.md) and add an entry.
If a later decision changes it, give the new decision its own number and link both records,
stating which part changed.

When a user-visible change ships, update the relevant [guide](docs/guide/README.md) in the same PR.
Keep wire-level contracts for other implementations in [reference](docs/reference/) and link to
them from the guide. When moving or retiring a page, update its index and check for references in
package READMEs and source comments.

### Formats by document type

For a repository file, copy its [Markdown template](docs/maintainers/document-templates.md) into
the destination file, replace the placeholders, and preview the result. Each template includes a
link to a finished example.

GitHub loads the [feature PRD](.github/ISSUE_TEMPLATE/feature-prd.md) through the issue chooser
and fills new pull request descriptions from the [pull request template](.github/pull_request_template.md).
Edit those files on `main` through a PR to change the prompts for future submissions.

## Automation

Bots and checks on every PR (Renovate, CodeRabbit, autofix.ci, CodeQL, zizmor, Codecov) and the
settings that enable them: [Repository automation](docs/maintainers/repository-automation.md).

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
