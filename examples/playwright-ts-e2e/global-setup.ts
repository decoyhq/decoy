import { once } from 'node:events'
import { createServer } from 'node:http'
import { setup } from '@decoyhq/playwright'
import type { SetupConfig } from '@decoyhq/playwright'

/** Fixed port of the example's own upstream, targeted by the Passthrough Behavior in mocks/. */
export const UPSTREAM_PORT = 4174

/** Starts the local Passthrough upstream, then Decoy; tears both down afterwards. */
export default async function globalSetup(config: SetupConfig) {
  const upstream = createServer((req, res) => {
    res
      .writeHead(200, { 'content-type': 'application/json' })
      .end(JSON.stringify({ name: `Upstream profile (${req.method} ${req.url})` }))
  })
  upstream.listen(UPSTREAM_PORT, '127.0.0.1')
  const [error] = await Promise.race([
    once(upstream, 'listening').then(() => [undefined]),
    once(upstream, 'error'),
  ])
  if (error) {
    throw new Error(`Passthrough upstream could not listen on 127.0.0.1:${UPSTREAM_PORT}: ${error}`)
  }

  const stopDecoy = await setup(config)
  return async () => {
    await stopDecoy()
    upstream.close()
  }
}
