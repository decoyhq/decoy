// A service worker that fetches on request: its own request must be routed like the page's.
globalThis.addEventListener('message', async (event) => {
  const response = await fetch('/api/users/1')
  const body = await response.json().catch(() => ({}))
  event.source.postMessage(response.ok ? body.name : `Error ${response.status}`)
})
