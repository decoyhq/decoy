// Serves the static app. It has no /api or /live handlers: the browser's API calls and its /live
// socket are routed to Decoy.
import { readFile } from 'node:fs/promises'
import { createServer } from 'node:http'

const port = Number(process.env.PORT ?? 4173)
const page = new URL('./index.html', import.meta.url)
const live = new URL('./live.html', import.meta.url)

createServer(async (req, res) => {
  if (req.url === '/' || req.url === '/index.html') {
    res.writeHead(200, { 'content-type': 'text/html' }).end(await readFile(page))
  } else if (req.url === '/live.html') {
    res.writeHead(200, { 'content-type': 'text/html' }).end(await readFile(live))
  } else {
    res.writeHead(404).end()
  }
}).listen(port, '127.0.0.1', () => console.log(`app on http://127.0.0.1:${port}`))
