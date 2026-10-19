// Serves the static app. It has no /api handlers: the browser's API calls are routed to Decoy.
import { readFile } from 'node:fs/promises'
import { createServer } from 'node:http'

const port = Number(process.env.PORT ?? 4173)
const page = new URL('./index.html', import.meta.url)

createServer(async (req, res) => {
  if (req.url === '/' || req.url === '/index.html') {
    res.writeHead(200, { 'content-type': 'text/html' }).end(await readFile(page))
  } else {
    res.writeHead(404).end()
  }
}).listen(port, '127.0.0.1', () => console.log(`app on http://127.0.0.1:${port}`))
