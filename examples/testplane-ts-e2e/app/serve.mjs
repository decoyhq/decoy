// Serves the static app. It has no /api handlers: the browser's API calls are routed to Decoy.
import { readFile } from 'node:fs/promises'
import { createServer } from 'node:http'

const port = Number(process.env.PORT ?? 4173)
const pages = {
  '/': new URL('./index.html', import.meta.url),
  '/index.html': new URL('./index.html', import.meta.url),
  '/popup.html': new URL('./popup.html', import.meta.url),
  '/worker.js': new URL('./worker.js', import.meta.url),
  '/shared.js': new URL('./shared.js', import.meta.url),
  '/sw.js': new URL('./sw.js', import.meta.url),
}

createServer(async (req, res) => {
  const page = pages[req.url]
  if (page) {
    const type = req.url.endsWith('.js') ? 'text/javascript' : 'text/html'
    res.writeHead(200, { 'content-type': type }).end(await readFile(page))
  } else {
    res.writeHead(404).end()
  }
}).listen(port, '127.0.0.1', () => console.log(`app on http://127.0.0.1:${port}`))
