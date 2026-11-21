import { useState } from 'react'

export function RequestDemo() {
  const [missing, setMissing] = useState(false)
  if (import.meta.env.SSG_MD) {
    return [
      '',
      '| Collection | GET /users/123 | Response body |',
      '| --- | --- | --- |',
      '| happy-path | 200 OK | `{"id":"123","name":"Ada"}` |',
      '| user-missing | 404 Not Found | `not found` |',
      '',
    ].join('\n')
  }
  return (
    <section className="request-demo" aria-label="Interactive mock response">
      <div className="demo-request">
        <b>GET</b> <code>/users/123</code>
      </div>
      <div className="demo-controls">
        <span>Collection</span>
        <div role="group" aria-label="Collection">
          {['happy-path', 'user-missing'].map((name) => (
            <button
              key={name}
              type="button"
              aria-pressed={missing === (name === 'user-missing')}
              onClick={() => {
                setMissing(name === 'user-missing')
              }}
            >
              {name}
            </button>
          ))}
        </div>
      </div>
      <div className="demo-response" aria-live="polite" aria-atomic="true">
        <strong className={missing ? 'missing' : ''}>{missing ? '404 Not Found' : '200 OK'}</strong>
        <pre>{missing ? 'not found' : '{\n  "id": "123",\n  "name": "Ada"\n}'}</pre>
      </div>
      <details>
        <summary>Show YAML</summary>
        <pre>{`- id: ${missing ? 'user-missing' : 'happy-path'}\n  routes: [users-by-id:user-123:${missing ? 'missing' : 'success'}]`}</pre>
      </details>
    </section>
  )
}
