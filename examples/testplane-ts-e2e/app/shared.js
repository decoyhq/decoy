// A shared worker that fetches on connect: its own request must be routed like the page's.
globalThis.addEventListener('connect', async (event) => {
  const port = event.ports[0]
  const response = await fetch('/api/users/1')
  const body = await response.json().catch(() => ({}))
  port.postMessage(response.ok ? body.name : `Error ${response.status}`)
})
