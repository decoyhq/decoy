# Quickstart

Mock `GET /users/123`, then switch it to a 404 without restarting.

`mocks/routes/users.yaml`, one [Route](routes.md):

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

`mocks/collections.yaml`, two [Collections](collections.md). The first one is the default:

```yaml
- id: happy-path
  routes:
    - users-by-id:user-123:success
- id: user-missing
  from: happy-path
  routes:
    - users-by-id:user-123:missing
```

Serve it:

```sh
decoy serve --routes mocks/routes --collections mocks/collections.yaml --port 8080
```

```sh
curl -i localhost:8080/users/123   # 200 {"id":"123","name":"Ada"}
curl -X POST localhost:8080/__decoy__/control/useCollection -H 'content-type: application/json' -d '{"collection":"user-missing"}'
curl -i localhost:8080/users/123   # 404 not found
curl -i localhost:8080/orders      # 501, x-mock-miss: true: nothing mocks it, nothing is proxied
```

Next: [Sessions and control](sessions-and-control.md) for per-test isolation, or
[Playwright](playwright.md) to drive it from browser tests.
