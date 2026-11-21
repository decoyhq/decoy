# AGENTS.md

Operating manual for agents working in the **decoy** monorepo.

## Read this before behavioral work

- `CONTEXT.md` is the normative glossary. Read it before changing schemas, public APIs, docs,
  adapters, or issue text that names Decoy concepts.
- Before writing a spec, ADR, or user guide, follow the [documentation placement rules](CONTRIBUTING.md#where-documentation-goes).
- Read the feature issue and applicable ADRs before implementing planned behavior.
- `docs/adr/` holds accepted architectural decisions. Read ADRs before reversing or replacing a
  recorded decision.
- Crates may carry their own `AGENTS.md` with layering rules (e.g. `crates/decoy-runtime/AGENTS.md`).
  Follow it when changing code in that crate.

## Agent skills

### Issue tracker

Issues and specs are tracked in GitHub Issues for `decoyhq/decoy`. See `docs/agents/issue-tracker.md`.

### Triage labels

Use the default five-label triage vocabulary. See `docs/agents/triage-labels.md`.

### Domain docs

This repo uses a single-context domain model: root `CONTEXT.md` plus root `docs/adr/`. See `docs/agents/domain.md`.

## Toolchain

pnpm 12 · Node 24 · Rust 1.99, pinned in `.prototools` (proto) and `rust-toolchain.toml`. Install with
`pnpm install` (use `--frozen-lockfile` in CI). pnpm settings live in `pnpm-workspace.yaml`, not `.npmrc`.
Dev dependencies used by several workspaces take their version from the `catalog:` in
`pnpm-workspace.yaml`; reference them as `"catalog:"` and bump them there.

## Commands

Prefer [`task`](https://taskfile.dev) (`Taskfile.yml`); each task wraps the plain command shown.

| task                           | runs                                                          |
| ------------------------------ | ------------------------------------------------------------- |
| `task check`                   | `lint` + `typecheck` + `test` — what CI gates on              |
| `task lint`                    | `cargo fmt --check`, `cargo clippy -D warnings`, `pnpm check` |
| `task fmt`                     | `cargo fmt`, `pnpm fix`                                       |
| `task typecheck`               | `pnpm typecheck` (TS 7 `tsc`)                                 |
| `task test`                    | `cargo test --workspace`, `pnpm test`                         |
| `task build`                   | `cargo build --workspace`, `pnpm build`                       |
| `task e2e`                     | `build`, then every example's `test:e2e`                      |
| `task release -- <ver>`        | open a release PR setting one version everywhere              |
| `task release:tag -- <ver>`    | after it merges, tag `origin/main` and push the tag           |
| `task release:latest -- <ver>` | mark that release Latest (install script, `decoy upgrade`)    |
| `task serve -- <args>`         | `cargo run -p decoy -- serve <args>`                          |

Examples (`examples/*`) are the e2e tier and run their own `test:e2e` per workspace.

## Conventions

- Conventional Commits; small vertical-slice PRs.
- `@decoyhq/*` is the package scope; the CLI bin is `decoy`.
- Don't reach for the real network in tests — Decoy is fail-closed by default.
