# Document templates

Copy the block for the document you are writing. Replace the placeholders, use headings that
name the actual task, and remove sections that do not apply. For a feature spec or pull request,
use the [PRD issue template](../../.github/ISSUE_TEMPLATE/feature-prd.md) or the
[pull request template](../../.github/pull_request_template.md).

## ADR

Use `docs/adr/NNNN-decision.md`. See [Session control commands](../adr/0005-session-control-commands.md).

```markdown
# NNNN. <Decision in a short sentence>

Accepted <YYYY-MM-DD>.

<Constraint that forced a choice. What alternative did we consider?>
<What did we choose, and why?>
<What does that choice make easier, more costly, or necessary?>

<If this amends an earlier record, link it and name the part that changed.>
```

## Project README

Use `README.md`. See [Decoy](../../README.md).

```markdown
# <Project name>

<Who uses this and what can they do with it?>

## Try it

1. <Install the tool, or link the installation instructions.>
2. <Run the smallest working example.>
3. <Show what success looks like.>

## Documentation

<Link the guide and runnable examples.>

## Contributing

<Link the contributor setup.>
```

## Documentation index

Use `README.md` in the directory it indexes. See [Guide index](../guide/README.md).

```markdown
# <Subject>

Start with <first task and link>.

## <What the reader wants to do>

- <Page link> — <what the reader will learn or do>.

## <Another task>

- <Page link> — <when to use it>.
```

## Tutorial or quickstart

Use `docs/guide/`. See [First mock](../guide/quickstart.md).

```markdown
# <Outcome to achieve>

You will <observable result>. You need <tools or starting files>.

## <First action>

In <working directory>, create <filename> with <contents>.

## <Run the example>

<Command to copy. Mention if it keeps running or needs a second terminal.>

## Check the result

<Verification command and expected output.>
<How to stop or clean up. Link the next task.>
```

## Task or integration guide

Use `docs/guide/`. See [Playwright](../guide/playwright.md).

```markdown
# <Task or integration>

Use this when <reader goal>. You need <prerequisites>.

## Set up <integration>

<Install command, configuration filename, and smallest working configuration.>

## <Exercise the integration>

<Code or command followed by the result to expect.>

## Options and limits

<Defaults, supported cases, and what happens outside those limits.>
<Link the runnable example.>
```

## Product reference

Use `docs/guide/`. See [Configuration](../guide/config.md).

```markdown
# <Command, schema, or API>

<What this configures or controls.>

## <Operation or field group>

| Field or argument | Type   | Default               | Meaning  |
| ----------------- | ------ | --------------------- | -------- |
| <name>            | <type> | <default or required> | <effect> |

<Minimal invocation or payload and its result.>

## Behavior

<Precedence, limits, errors, and what the caller observes.>
```

## Implementation contract

Use `docs/reference/`. See [Plugin protocol](../reference/plugin-protocol.md).

```markdown
# <Protocol name and version>

<Participants and scope.>

## Lifecycle and framing

<Who initiates, how data is encoded and delimited, and how the exchange ends.>

## Messages

| Message | Sender        | Fields and types               | Reply           |
| ------- | ------------- | ------------------------------ | --------------- |
| <kind>  | <participant> | <required and optional fields> | <reply or none> |

<One valid exchange. Specify ordering and correlation rules.>

## Failure behavior

<How each participant handles invalid data, timeouts, limits, and disconnects.>
```

## Package README

Use `packages/<name>/README.md`. See [Plugin SDK](../../packages/plugin/README.md).

```markdown
# <Package name>

<What the public API lets the caller do.>

## Install

<Package manager command and runtime prerequisites.>

## Use

<Smallest complete example using the public import, with its expected effect.>
<Link configuration or protocol details using URLs that work on the package registry.>
```

## Example README

Use `examples/<name>/README.md`. See [WebSocket example](../../examples/ws-node/README.md).

```markdown
# <Example name>

<Which client calls Decoy, what it sends, and what this demonstrates.>

## Run

<Prerequisites and exact command from the repository root.>

## What to observe

<Expected response or test outcome. Explain which change triggers a different result.>
<Link the relevant guide.>
```

## Benchmark report

Keep this report alongside the measured example. See [Transformer benchmarks](../../examples/ws-plugin-node/transformer-benchmarks.md).

```markdown
# <Question being measured>

## Reproduce

<Revision, versions, machine, workload, concurrency, warmup, and number of runs.>
<Exact commands for the baseline and comparison.>

## Results

| Configuration | <Metric with units> | <Variation across runs> |
| ------------- | ------------------- | ----------------------- |
| <Baseline>    | <value>             | <spread>                |
| <Comparison>  | <value>             | <spread>                |

<What the measurement supports and which conditions limit the comparison.>
```

## Glossary entry

Use `CONTEXT.md` under the relevant subject. See [Decoy terms](../../CONTEXT.md).

```markdown
**<Canonical term>**:
<Define what the term means in Decoy. Mention the distinction a reader needs to use it correctly.>
_Avoid_: <confusable terms and why>.
```

## Contributor guide

Use `CONTRIBUTING.md`. See [Contributing](../../CONTRIBUTING.md).

```markdown
# Contributing to <project>

## Toolchain and setup

<Required versions, installation command, and how to check setup succeeded.>

## Checks

| Command   | What it verifies  |
| --------- | ----------------- |
| <command> | <expected result> |

## Submitting a change

<Repository conventions and how to open a PR.>

## Releasing

<Who runs the release steps, when, and where the procedure lives.>
```

## Maintainer runbook

Use `docs/maintainers/`. See [Repository automation](repository-automation.md).

```markdown
# <Operation>

Run this when <trigger>. You need <access, tools, and starting state>.

## Procedure

1. <Action, including where to run it.>
2. <Next action and how to tell it succeeded.>

## Verify

<Check the observable final state.>

## Recovery

<If applicable, explain how to stop safely, retry, or undo a partial change.>
```

## Agent instructions

Use `AGENTS.md` at the applicable scope, or `docs/agents/` for workflow details. See [Runtime layering rules](../../crates/decoy-runtime/AGENTS.md).

```markdown
# <Repository, directory, or workflow>

These instructions cover <scope>.

## <Task or decision>

- When <condition>, <specific action>. Consult <canonical file> for <reason>.
- Verify <observable result> with <command or check>.
```
