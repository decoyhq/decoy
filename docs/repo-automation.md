# Repository automation

What runs on every PR, and the one-time settings a maintainer switches on for it.

| Need                         | Tool                                          | Lives in                                        |
| ---------------------------- | --------------------------------------------- | ----------------------------------------------- |
| Dependency updates           | Renovate                                      | `renovate.json` + GitHub App                    |
| Security advisories          | Dependabot alerts, `cargo-deny`, `pnpm audit` | repo settings, `deny.toml`, `ci.yml`            |
| Review on every PR           | CodeRabbit                                    | `.coderabbit.yaml` + GitHub App                 |
| Formatting                   | autofix.ci                                    | `.github/workflows/autofix.yml` + GitHub App    |
| Conventional Commit PR title | action-semantic-pull-request                  | `.github/workflows/pr-title.yml`                |
| Code security                | CodeQL (Actions, JS/TS, Rust)                 | `.github/workflows/codeql.yml`                  |
| Workflow security            | zizmor                                        | `.github/workflows/zizmor.yml`                  |
| Coverage                     | Codecov                                       | `.github/workflows/coverage.yml`, `codecov.yml` |
| Leaked secrets               | secret scanning + push protection             | repo settings                                   |

## One-time setup

### 1. Install the GitHub Apps

For each app: open the link, **Install** (or **Configure** if already installed for your account),
choose **Only select repositories → decoy**, save. All are free for public repositories.

1. **Renovate** — <https://github.com/apps/renovate>. `renovate.json` is already in the repo, so
   there is no onboarding PR: Renovate opens a **Dependency Dashboard** issue and its first PRs on
   the next Monday-morning run (or right away from the dashboard's checkboxes). Its first PR pins
   every action to a commit SHA.
2. **CodeRabbit** — <https://github.com/apps/coderabbitai> (or sign in at <https://coderabbit.ai>
   with GitHub and add the repo). It reviews every non-draft PR; reply `@coderabbitai` in a PR to
   ask it something, `@coderabbitai review` to re-run.
3. **autofix.ci** — <https://github.com/apps/autofix-ci>. Without it the `autofix.ci` job still
   runs but can't push the fix.
4. **Codecov** — <https://github.com/apps/codecov>, then sign in at <https://app.codecov.io> with
   GitHub and open `what3verCODE/decoy` once. Uploads use OIDC, so no `CODECOV_TOKEN` secret.

### 2. Repository settings

**Settings → Advanced Security** (called "Code security" on some accounts):

- **Dependabot alerts**: on. **Dependabot security updates**: on (security fixes only; version
  updates come from Renovate, so there is no `.github/dependabot.yml`).
- **Secret scanning** and **Push protection**: on.
- **Code scanning → CodeQL analysis**: leave **Default setup off**. The `CodeQL` workflow is the
  advanced setup, and GitHub refuses to run both.

**Settings → General → Pull Requests**:

- Allow **squash merging** with **Default commit message: Pull request title** (the title is what
  `PR title` checks).
- Optional: **Allow auto-merge**, so a PR merges once its required checks pass.

**Settings → Rules → Rulesets → New branch ruleset** for `main` (optional but recommended):
require a pull request and the status checks `ci`, `rust`, `cargo-deny`, `conventional` and the
`e2e` jobs. Don't make CodeRabbit, Codecov or CodeQL required; they report, they don't gate.

### 3. Check it works

Open a draft PR with a badly formatted change and a title like `update stuff`, then mark it
ready for review. Expect:

| Check        | Expected                                                                |
| ------------ | ----------------------------------------------------------------------- |
| `PR title`   | fails until the title is a Conventional Commit, e.g. `chore: demo bots` |
| `autofix.ci` | pushes a `[autofix.ci] apply automated fixes` commit                    |
| CodeRabbit   | posts a summary and review comments                                     |
| `CodeQL`     | three analyses; findings appear under Security → Code scanning          |
| `zizmor`     | runs when `.github/**` changes; findings go to Code scanning            |
| `Coverage`   | Codecov comments with Rust and JS coverage                              |

Renovate shows up separately: the Dependency Dashboard issue and its PRs.
