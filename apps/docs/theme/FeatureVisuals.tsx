export function RoutePreview() {
  return (
    <div className="feature-frame feature-example">
      <div className="feature-screen route-editor">
        <div className="preview-titlebar">
          <span className="preview-window-controls" aria-hidden="true" />
          Route editor
        </div>
        <div className="route-editor-tab">
          <span aria-hidden="true">{'{}'}</span>
          <code>mocks/routes/users.yaml</code>
        </div>
        <pre title="mocks/routes/users.yaml">
          <code>
            {`id: users-by-id
transport: http
match:
  method: GET
  path: /api/users/{id}

cases:
  any:
    behaviors:\n`}
            <span className="route-behavior">{`      missing:
        status: 404
        body: not found`}</span>
          </code>
        </pre>
      </div>
      <div className="feature-screen route-response">
        <div className="preview-titlebar">
          <span className="preview-window-controls" aria-hidden="true" />
          HTTP response
        </div>
        <div className="route-response-request">
          <code>GET /api/users/1</code>
          <code className="route-response-status">404 Not Found</code>
        </div>
        <div className="route-response-body">
          <span>Body</span>
          <code>not found</code>
        </div>
      </div>
    </div>
  )
}

export function ScenarioPreview() {
  return (
    <div className="feature-frame">
      <div className="feature-screen terminal-trace">
        <div className="feature-screen-title">
          <span className="trace-prompt" aria-hidden="true">
            &gt;_
          </span>{' '}
          Decoy terminal UI
        </div>
        <div className="trace-row">
          <code>GET /api/users/1</code>
          <code className="trace-ok">200 OK</code>
        </div>
        <div className="trace-command">
          <code>
            <span className="trace-prompt" aria-hidden="true">
              &gt;{' '}
            </span>
            /collection no-users
          </code>
        </div>
        <div className="trace-row">
          <code>GET /api/users/1</code>
          <code className="trace-not-found">404 Not Found</code>
        </div>
      </div>
    </div>
  )
}

export function SessionsPreview() {
  return (
    <figure className="feature-frame">
      <figcaption className="session-request">
        <code>GET /api/users/1</code>
      </figcaption>
      <div className="session-lanes">
        <div className="feature-screen session-lane">
          <strong>Session A</strong>
          <code>happy-path</code>
          <code className="trace-ok">200 OK</code>
        </div>
        <div className="feature-screen session-lane">
          <strong>Session B</strong>
          <code>no-users</code>
          <code className="trace-not-found">404 Not Found</code>
        </div>
      </div>
    </figure>
  )
}
