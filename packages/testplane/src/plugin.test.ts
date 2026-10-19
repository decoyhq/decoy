import { copyFileSync, mkdtempSync } from 'node:fs'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { isHealthy } from '@decoyhq/runner-core'
import { FAKE_DECOY } from '@decoyhq/runner-core/testing'
import { describe, expect, test } from '@rstest/core'
import type Testplane from 'testplane'
import decoyPlugin from './plugin'

async function freePort(): Promise<number> {
  const server = createServer()
  await new Promise<void>((done) => server.listen(0, '127.0.0.1', done))
  const address = server.address()
  await new Promise((done) => server.close(done))
  if (address === null || typeof address === 'string') {
    throw new Error('expected a TCP address')
  }
  return address.port
}

/** The master-process slice of Testplane the plugin uses, with a way to fire its events. */
function fakeMaster(configPath: string) {
  const handlers = new Map<string, () => Promise<void>>()
  const testplane = {
    isWorker: () => false,
    events: { RUNNER_START: 'startRunner', RUNNER_END: 'endRunner' },
    config: { configPath },
    on: (event: string, handler: () => Promise<void>) => handlers.set(event, handler),
  }
  return {
    testplane: testplane as unknown as Testplane,
    fire: (event: string) => handlers.get(event)?.() ?? Promise.resolve(),
  }
}

describe('decoyPlugin (master)', () => {
  test("runs `command` from the config file's directory, then stops it at the end", async () => {
    const dir = mkdtempSync(join(tmpdir(), 'decoy-testplane-'))
    copyFileSync(FAKE_DECOY, join(dir, 'fake-decoy.mjs'))
    const port = await freePort()
    const url = `http://127.0.0.1:${port}`
    const { testplane, fire } = fakeMaster(join(dir, '.testplane.conf.ts'))

    // A relative script path only resolves if `command` runs in `dir`.
    decoyPlugin(testplane, {
      url,
      command: `node fake-decoy.mjs ${port}`,
      reuseExistingServer: false,
    })
    await fire('startRunner')
    expect(await isHealthy(url)).toBe(true)

    await fire('endRunner')
    expect(await isHealthy(url)).toBe(false)
  })
})
