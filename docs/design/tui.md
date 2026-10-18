# Interactive terminal UI

Status: working design note, agreed in #285. Implementation is stacked across #286-#294.

## Problem

In local dev, changing the active Collection or overriding a Route means hand-crafting `curl`
calls to the Control API. Seeing what Decoy did with a request means reading the mock-miss body in
the browser network tab. Nothing shows the live traffic, and nothing explains _why_ a request hit
or missed without replaying the matcher by hand.

## Shape

Bare `decoy` starts the runtime **plus** an interactive terminal UI in the same process. It looks
and feels like an agent CLI (Claude Code / Codex): a live log of requests in the terminal's normal
scrollback, an input line with slash commands and autocomplete, and a status bar. `decoy serve`
does not change and stays headless.

## Entry

- `decoy` (no subcommand) = `serve` + TUI. Same flags and config discovery as `serve`.
- `decoy serve` stays headless, with output unchanged.
- No TTY on stdin/stdout → exit non-zero with `decoy: no terminal; use \`decoy serve\``.

## Rendering

Inline, not fullscreen.

- Log lines get printed into the terminal's normal scrollback, and only the bottom region is live
  (status bar, input, autocomplete popup, picker). ratatui `Viewport::Inline` + `insert_before`.
- The terminal owns scroll, selection, copy and search. No mouse capture, no alternate screen.
- A printed line is final. A request is printed once its response finishes. Long-lived connections
  (WS/SSE) print separate `open` / `close` lines.
- Reference implementation: Codex CLI (`codex-rs/tui`), which uses the same stack.

## Status bar

`listen addr · session · collection · N overrides`.

## Log lines

- One line per finished request, each with a short id:
  `#41 12:01:03 GET /users/123 → users-by-id:user-123:success 200 4ms`
- Miss: red `MISS` + reason. Passthrough: yellow, with an `→ upstream` tag. Hit: green.
- Requests from a Session other than the active one get the tag `[sess:x]`.
- Runtime events (startup, listen address, warnings, errors, reload results) show as dim system
  lines. While the TUI runs, tracing/stderr output goes into the log so it can't corrupt the screen.
- User commands are echoed with a `›` marker.
- `NO_COLOR` is respected.

## Commands

The Controller verbs, in Decoy vocabulary.

| command                        | effect                                                     |
| ------------------------------ | ---------------------------------------------------------- |
| `/collection <id>`             | `useCollection` on the active Session                      |
| `/route <route:case:behavior>` | `useRoute` on the active Session                           |
| `/reset`                       | `reset`: clears route overrides, keeps the Collection      |
| `/session <id>`                | switch which Session the commands target (default: global) |
| `/sessions`                    | list the Sessions that currently hold a Selection          |
| `/status`                      | print the active Session's Collection + full override list |
| `/show <id>`                   | print the full request record (see Request detail)         |
| `/reload`                      | reload config (see Reload)                                 |
| `/help`                        | list commands                                              |
| `/quit`                        | exit                                                       |

- Input without a leading `/` is reserved and prints a hint.
- Autocomplete popup (agent-CLI style): Tab / ↑↓ select, Enter accepts. `/route` completes one
  segment at a time (`route` → `case` → `behavior`) from the live catalog. `/collection` and
  `/session` complete from live state.
- Input history: ↑/↓ when the popup is closed, in memory, one run only.

## Keys

Ctrl+C clears the input, and a second Ctrl+C on empty input quits. Ctrl+D on empty input
quits. Esc closes the popup or picker. Ctrl+O opens the request picker.

## Sessions

Sessions are implicit (`Controller` creates a Selection on its first mutation).

- Switching away does nothing to the previous Session.
- `/session <unknown>` is allowed and shows a `new session` hint. The Session reads as defaults
  until the first mutation.
- When the active Session is ended by someone else (e.g. Playwright teardown), print the system
  line `session x ended`. The next command reads it from defaults again; a Selection is created
  only on its first mutation.

## Request detail

`/show <id>` (or Ctrl+O picker → Enter) prints the record into scrollback as plain indented text
with stable labels and no box-drawing, readable by a human or by an agent in a neighbouring pane.

- Request: method, URL, headers, body, Session.
- Match explanation: a reason for each candidate, for hits _and_ misses. Candidates are listed in
  evaluation order (bottom-to-top), each with why it was rejected (`method`, `path`, or the first
  failed Case condition with expected vs actual). A hit also shows the winning address, the
  conditions that passed, and the captured `pathParams`.
- Response: status, headers, body. Passthrough: upstream URL + upstream status.
- Timing.
- Bodies are shown in their most-decoded form: Pipeline-decoded protobuf/gRPC as JSON, JSON
  pretty-printed, other text as-is, `<binary, N bytes>` only when nothing decodes it.
- Retention: the last 500 records; each body capped at 64KB of rendered output, with
  `…truncated N bytes`. `/show` on an evicted id → `#12 expired`.

## Reload

- `/reload` re-reads all config and hot-swaps everything except listeners (port, host, listeners,
  TLS). If a listener field changed, everything else is applied and the TUI warns
  `listener changes need restart`. The server never restarts and no connections drop.
- Selections are kept. Overrides whose address no longer exists are dropped, each with a warning
  line. A Session whose Collection no longer exists falls back to the default. Sessions that never
  set a Collection follow a new default `collection`.
- Parse or validation failure → the old config stays live and the error is printed.
- Once plugins exist, reload restarts them.
- `--watch` (on `decoy` and `decoy serve`) triggers the same reload on file changes.

## Architecture

- Crates: `decoy` (bin: clap, `main.rs`, `upgrade`), `decoy-runtime` (lib: engine + server),
  `decoy-tui` (lib: ratatui + crossterm, depends on the runtime).
- `decoy_runtime::serve(config, shutdown)` binds every listener and returns a `Server` with the
  bound `listeners`, startup `notices` (e.g. the auto TLS CA path) and the shared `controller`
  (`Arc<RwLock<Controller>>`); `Server::wait()` serves until `shutdown` resolves, then drains.
  `serve()` prints nothing: `decoy serve` prints the notices and `listening on` lines from
  `Server`, and the TUI shows them as system lines. `decoy serve` passes Ctrl-C/SIGTERM; the TUI
  passes its own quit signal too.
- `Server::reloader()` returns a `Reloader`. `Reloader::reload(read_config)` reads the config,
  builds the new Catalog off the lock, swaps it into the `Controller` (which keeps the
  Selections, see Reload) and returns the warnings. Reloads run one at a time. `decoy_tui::run`
  gets a closure that re-reads the config from the same flags for `/reload`.
- `decoy_runtime::watch::ConfigWatcher` watches the config file's and collections file's parent
  directories (the files themselves when polling), descriptor sets, and the `.yaml`/`.proto` files
  in the routes and proto trees (with `notify`; polling where native events don't arrive or can't
  be set up). `changed()` resolves once per burst after 300ms of quiet, then re-arms from the
  config as it is now, so a recreated directory or moved `routes:` stays watched. The caller runs
  a reload per signal: `decoy serve` prints the result to stderr, the TUI as system lines.
- The TUI talks to the in-process `Controller` and subscribes to an in-process event broadcast
  (`RequestEvent`, `SessionEvent`, `LogEvent`). The Control API does not change.
- Runtime warnings and errors are `Event::Log` on the server's broadcast (a task-local set around
  each request and accept loop), and go to stderr only while nobody subscribes, so headless output
  is unchanged and nothing draws over the TUI. No fd redirection, so it works on Windows.
- `Server::subscribe()` (or `Controller::subscribe()`) returns a `tokio::sync::broadcast` receiver
  of `decoy_runtime::events::Event`. With no receiver nothing is captured; a receiver more than
  `events::CAPACITY` events behind gets `Lagged` and loses the oldest. WS/SSE connection events
  arrive with those transports.

## Testing

- Unit tests for the command parser and completion.
- insta snapshot tests (committed) of the live region and of rendered log/detail lines in key
  states, so every UI change shows up as a diff in review.
- Scripted key-event tests: feed keys into the app state and assert the resulting Controller calls.
- Reload and watch tests on temp config dirs.
- No PTY e2e in this effort.

## Out of scope

- Attaching a TUI to an already running runtime (`decoy tui --url`) and any event-stream Control
  API endpoint.
- Fullscreen mode, mouse support, in-app search/filter.
- Editing routes/collections YAML from the TUI.
- A dedicated Sessions panel or e2e debugging workflow (the Playwright network tab covers it).
- Free-text (non-`/`) input.
- JSON output of `/show`.
