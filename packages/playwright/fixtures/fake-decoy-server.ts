import { createServer, type IncomingMessage, type ServerResponse } from 'node:http'

export interface ControlCall {
  path: string
  session: string | undefined
  body: string
}

/**
 * In-process stand-in for native Decoy in package tests: answers health, records Control API calls,
 * and replies to each call with `reply` (default 200).
 */
export async function startFakeDecoy(
  reply: (call: ControlCall) => { status: number; body: string } = () => ({
    status: 200,
    body: '{}',
  }),
) {
  const calls: ControlCall[] = []
  const server = createServer((req: IncomingMessage, res: ServerResponse) => {
    let body = ''
    req.on('data', (chunk: Buffer) => {
      body += chunk.toString()
    })
    req.on('end', () => {
      if (req.url === '/__decoy__/health') {
        res.writeHead(200).end('{"status":"ok"}')
        return
      }
      const session = req.headers['x-mock-session']
      const call = {
        path: req.url ?? '',
        session: typeof session === 'string' ? session : undefined,
        body,
      }
      calls.push(call)
      const { status, body: replyBody } = reply(call)
      res.writeHead(status, { 'content-type': 'application/json' }).end(replyBody)
    })
  })
  await new Promise<void>((done) => server.listen(0, '127.0.0.1', done))
  const address = server.address()
  if (address === null || typeof address === 'string') {
    throw new Error('expected a TCP address')
  }
  return {
    url: `http://127.0.0.1:${address.port}`,
    calls,
    close: () =>
      new Promise<void>((done) =>
        server.close(() => {
          done()
        }),
      ),
  }
}
