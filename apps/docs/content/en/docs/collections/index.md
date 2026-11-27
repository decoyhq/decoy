# Collections

A **Collection** lists the Behaviors that are active together for a Scenario. All Collections
live in one YAML file.

```yaml title="collections.yaml"
- id: happy-path
  routes:
    - users-by-id:user-1:ada
    - profile:any:upstream

- id: no-users
  from: happy-path
  routes:
    - users-by-id:user-1:missing
```

- Each entry is a **Behavior address**, `route:case:behavior`. The structured form
  `{ route: users-by-id, case: user-1, behavior: ada }` works too. There are no implicit defaults:
  a Route whose Cases aren't listed is not active.
- `from` inherits another Collection. A child entry for the same `route+case` replaces the parent's.
- The **first** Collection is the default. Override it at startup with
  `decoy serve --collection <id>` (or `collection:` in the [config file](../config/index.md)), or at runtime
  per Session with the [Control API](../sessions-and-control/index.md).

## Order matters

Active entries are checked bottom-to-top, and the last matching Case wins. Put broad fallbacks
first and specific Cases after them:

```yaml title="collections.yaml (excerpt)"
- id: z-default
  routes:
    - users:any:fallback # pathParams id: '*'
    - users-by-id:user-123:success # wins for /users/123
```
