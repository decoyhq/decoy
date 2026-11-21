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

Keep the PRD in the [GitHub issue tracker](https://github.com/decoyhq/decoy/issues). Put the user
problem, acceptance criteria, worked examples, and unresolved questions in the issue. This is the
spec that `to-spec` publishes and the implementation PR links to.

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

Use these outlines when creating a document or rewriting one. The order describes what the reader
needs first. Use headings that name the actual task or decision.

| Document and example                                                                           | What to include, in order                                                                                                                                                                                                                                                                       |
| ---------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Feature PRD · [WebSocket Routes issue](https://github.com/decoyhq/decoy/issues/416)            | Problem statement, solution, user stories with observable outcomes, implementation decisions, testing decisions, and out of scope. Use further notes for open questions and related issues. Publish the PRD as an issue.                                                                        |
| ADR · [Session control commands](docs/adr/0005-session-control-commands.md)                    | Number and decision in the title, acceptance date, constraint that forced the choice, alternative considered, decision and consequences. Link related or superseding records.                                                                                                                   |
| Project README · [Decoy](README.md)                                                            | Say who uses the tool and what it does. Link installation and the first working example, then the guide and contributor entry points.                                                                                                                                                           |
| Documentation index · [guide index](docs/guide/README.md)                                      | Start with the usual first step. Group links by reader task, with one line explaining what each destination helps them do. Use a numbered table for an ADR index.                                                                                                                               |
| Tutorial or quickstart · [first mock](docs/guide/quickstart.md)                                | Name the outcome and prerequisites. Give the files and commands in execution order, including working directories where they matter. Show the expected result, how to stop, and where to go next.                                                                                               |
| Task or integration guide · [Playwright](docs/guide/playwright.md)                             | State when to use it, list prerequisites, show the smallest working configuration, then explain the options, defaults, and limits. Link a runnable example when one exists.                                                                                                                     |
| Product reference · [configuration](docs/guide/config.md)                                      | Identify the command, schema, or API. Group fields or operations by subject, give types or payloads and defaults, and explain precedence, errors, and limits with examples. Keep this alongside the guides that use it.                                                                         |
| Implementation contract · [plugin protocol](docs/reference/plugin-protocol.md)                 | Identify participants and version. Specify lifecycle, framing, field types, message order, and failure behavior precisely enough for someone to write a compatible implementation.                                                                                                              |
| Package README · [plugin SDK](packages/plugin/README.md)                                       | Name the package and its purpose, show a minimal use of its public API, and link setup or protocol details. Use links that work from the published package page.                                                                                                                                |
| Example README · [WebSocket example](examples/ws-node/README.md)                               | Describe the client and what it demonstrates. Give prerequisites and exact commands from the repository root. Say what the reader should observe, then link the guide.                                                                                                                          |
| Benchmark report · [Transformer benchmarks](examples/ws-plugin-node/transformer-benchmarks.md) | State the question, environment, versions, workload and commands. Present results with units, then describe what the measurements establish and where they stop applying. Keep the report with the measured example.                                                                            |
| Glossary · [Decoy terms](CONTEXT.md)                                                           | Group entries by subject. Give the canonical term, its meaning in Decoy, and any easily confused terms. Use the same spelling in issues, code, and guides.                                                                                                                                      |
| Contributor guide · [Contributing](CONTRIBUTING.md)                                            | Start with prerequisites and setup. List the checks contributors run and what each verifies, then cover conventions and release tasks. Link maintainer procedures.                                                                                                                              |
| Maintainer runbook · [repository automation](docs/maintainers/repository-automation.md)        | State when to run the procedure, required access, and the systems involved. Give ordered steps and a concrete check that confirms success. Include recovery steps for operations that need them.                                                                                                |
| Agent instructions · [runtime rules](crates/decoy-runtime/AGENTS.md)                           | State the directory or workflow in scope, then the rules and triggers for consulting more detail. Put tracker commands, label mappings, or terminology policy in the corresponding topic under [agent docs](docs/agents/). Point tool-specific instruction files to the canonical instructions. |

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
