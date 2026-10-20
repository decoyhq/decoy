// A dedicated worker that fetches on start: its request must be routed like the page's.
const response = await fetch('/api/users/1')
const body = await response.json().catch(() => ({}))
globalThis.postMessage(response.ok ? body.name : `Error ${response.status}`)
