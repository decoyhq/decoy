# Terminal UI

Run `decoy` with no subcommand to start the server with an interactive log of every request. It
takes the same flags as `decoy serve`. Type `/` to run a command. Commands act on the active
Session, which is `default` until you pick another with `/session`.

## Commands

| command                        | does                                                       |
| ------------------------------ | ---------------------------------------------------------- |
| `/collection <id>`             | switch the active Session's Collection                     |
| `/route <route:case:behavior>` | pick the Behavior for a Route's Case in the active Session |
| `/reset`                       | clear the active Session's overrides, keep its Collection  |
| `/session <id>`                | switch the Session commands target; a new id starts fresh  |
| `/sessions`                    | list the Sessions that hold a Selection                    |
| `/status`                      | show the active Session's Collection and overrides         |
| `/show <id>`                   | open one request in the detail panel                       |
| `/reload`                      | re-read the config without restarting                      |
| `/help`                        | list commands and keys                                     |
| `/quit`                        | stop the server and exit                                   |

## Reload

`/reload` re-reads the config from the same flags and files `decoy` started with, then serves the
new Routes, Collections and settings. A request takes `maxBodyBytes` when it arrives and the Routes
and Selections once its body is read, so one whose body is still uploading during a reload keeps
the old limit but is matched against the new config. If the new config doesn't load, `/reload`
prints the error and the old config keeps serving.

Listeners stay bound as they started: changes to the port, host, `listeners` or TLS need a restart,
and `/reload` warns `listener changes need restart`.

Every Session keeps its Selection, with three exceptions:

- A Session that never picked a Collection follows the new default Collection.
- A Session whose picked Collection is gone falls back to the default Collection, with a warning.
- An override whose `route:case:behavior` address no longer exists is dropped, with a warning.

## Keys

| key                               | does                                                            |
| --------------------------------- | --------------------------------------------------------------- |
| `↑` `↓`                           | move through the completion popup; with no popup, input history |
| `Tab` / `→`                       | complete the highlighted item                                   |
| `Enter`                           | run the line; with the popup open, complete and run when whole  |
| `Esc`                             | close the popup or the detail panel                             |
| `Ctrl+O`                          | pick from recent requests                                       |
| click a request                   | open its details (`PgUp`/`PgDn` scroll them)                    |
| `PgUp` `PgDn` `Home` `End`, wheel | scroll the log                                                  |
| drag                              | select and copy; `Shift`+drag uses the terminal's own selection |
| `Ctrl+U`                          | clear the line                                                  |
| `Ctrl+C`                          | clear the line; twice quits                                     |
| `Ctrl+D`                          | quit, on an empty line                                          |

`Enter` after `/session <id>` switches to exactly the id typed, even when the popup offers a longer
seen one; press `Tab` first to take the suggestion.
