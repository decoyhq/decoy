# Quickstart

Mock `GET /users/123`, then switch it to a 404 without restarting.

[Install Decoy](install.md) first. The commands below use a POSIX shell and curl. If you installed
with npm, run them from your project directory and use `npx decoy` in place of `decoy`.

## Create the mock

Create the directory for your [Route](routes.md).

```sh
mkdir -p mocks/routes
```

Save the following as `mocks/routes/users.yaml`. The Route matches `GET /users/{id}`, and the
`user-123` Case defines two possible responses for ID `123`.

```yaml
id: users-by-id
transport: http
match:
  method: GET
  path: /users/{id}
cases:
  user-123:
    match:
      pathParams:
        id: '123'
    behaviors:
      success:
        status: 200
        body: { id: '123', name: Ada }
      missing:
        status: 404
        body: not found
```

Save these two [Collections](collections.md) as `mocks/collections.yaml`. Decoy starts with the first one.

```yaml
- id: happy-path
  routes:
    - users-by-id:user-123:success
- id: user-missing
  from: happy-path
  routes:
    - users-by-id:user-123:missing
```

## Serve the mock

From the directory containing `mocks/`, start Decoy and leave it running.

```sh
decoy serve --routes mocks/routes --collections mocks/collections.yaml --port 8080
```

In a second terminal, request the user, switch the Collection, and repeat the request.

```sh
curl -i localhost:8080/users/123   # 200 {"id":"123","name":"Ada"}
curl -X POST localhost:8080/__decoy__/control/useCollection -H 'content-type: application/json' -d '{"collection":"user-missing"}'
curl -i localhost:8080/users/123   # 404 not found
curl -i localhost:8080/orders      # 501 with x-mock-miss: true
```

Stop the server with Ctrl-C. See [Sessions and control](sessions-and-control.md) for per-test isolation, or
[Playwright](playwright.md) or [Testplane](testplane.md) to drive it from browser tests.
