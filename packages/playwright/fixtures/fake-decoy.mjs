// Stand-in for `decoy serve` in package tests: answers the liveness probe, with its pid, on the
// given port.
import { createServer } from 'node:http'

const port = Number(process.argv[2])
createServer((req, res) => {
  if (req.url === '/__decoy__/health') {
    res
      .writeHead(200, { 'content-type': 'application/json' })
      .end(JSON.stringify({ status: 'ok', pid: process.pid }))
  } else {
    res.writeHead(501).end()
  }
}).listen(port, '127.0.0.1')
