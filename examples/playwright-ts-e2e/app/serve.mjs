// Serves the static app. It has no /api or /live handlers: the browser's API calls and its /live
// socket are routed to Decoy.
import { readFile } from 'node:fs/promises'
import { createServer } from 'node:http'

const port = Number(process.env.PORT ?? 4173)
const page = new URL('./index.html', import.meta.url)
const live = new URL('./live.html', import.meta.url)

// GET /__rendezvous/<name>?parties=N holds each request until N requests with that name are
// waiting, then answers them all 200; one still waiting after 15s gets 408. Tests use it to run
// in step.
const RENDEZVOUS_TIMEOUT_MS = 15_000
const rendezvous = new Map()

function meet(res, name, parties) {
  const waiting = rendezvous.get(name) ?? []
  rendezvous.set(name, waiting)
  const timer = setTimeout(() => {
    waiting.splice(waiting.indexOf(party), 1)
    res.writeHead(408).end(`only ${waiting.length + 1} of ${parties} parties arrived`)
  }, RENDEZVOUS_TIMEOUT_MS)
  const party = { res, timer }
  waiting.push(party)
  if (waiting.length >= parties) {
    rendezvous.delete(name)
    for (const { res, timer } of waiting) {
      clearTimeout(timer)
      res.writeHead(200).end()
    }
  }
}

createServer(async (req, res) => {
  const url = new URL(req.url ?? '/', 'http://localhost')
  if (url.pathname === '/' || url.pathname === '/index.html') {
    res.writeHead(200, { 'content-type': 'text/html' }).end(await readFile(page))
  } else if (url.pathname === '/live.html') {
    res.writeHead(200, { 'content-type': 'text/html' }).end(await readFile(live))
  } else if (url.pathname.startsWith('/__rendezvous/')) {
    meet(res, url.pathname, Number(url.searchParams.get('parties') ?? 2))
  } else {
    res.writeHead(404).end()
  }
}).listen(port, '127.0.0.1', () => console.log(`app on http://127.0.0.1:${port}`))
