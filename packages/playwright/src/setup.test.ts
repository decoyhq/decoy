import { createServer, type Server } from 'node:http'
import { isHealthy } from '@decoyhq/runner-core'
import { FAKE_DECOY } from '@decoyhq/runner-core/testing'
import { defineConfig, type FullConfig } from '@playwright/test'
import { describe, expect, test } from '@rstest/core'
import { expectTypeOf } from 'expect-type'
import type { DecoyTestOptions } from './fixtures'
import type { DecoyOptions } from './options'
import { type SetupConfig, setup } from './setup'

describe('config typing', () => {
  test('a typed Playwright config fits setup', () => {
    expectTypeOf<FullConfig<DecoyTestOptions>>().toExtend<SetupConfig>()
  })

  test('users type decoyOptions with defineConfig<DecoyTestOptions>', () => {
    const config = defineConfig<DecoyTestOptions>({
      use: { decoyOptions: { url: 'http://127.0.0.1:8080' } },
    })
    expect(config.use?.decoyOptions?.url).toBe('http://127.0.0.1:8080')
  })
})

function config(decoyOptions: DecoyOptions | undefined): SetupConfig {
  return { projects: [{ use: { decoyOptions } }] }
}

async function listen(server: Server): Promise<number> {
  await new Promise<void>((done) => server.listen(0, '127.0.0.1', done))
  const address = server.address()
  if (address === null || typeof address === 'string') {
    throw new Error('expected a TCP address')
  }
  return address.port
}

async function freePort(): Promise<number> {
  const server = createServer()
  const port = await listen(server)
  await new Promise((done) => server.close(done))
  return port
}

describe('setup', () => {
  test('does nothing when Decoy is not configured', async () => {
    const teardown = await setup(config(undefined))
    await teardown()
  })

  test('starts one Decoy per distinct url across projects', async () => {
    const [first, second] = [await freePort(), await freePort()]
    const options = (port: number) => ({
      url: `http://127.0.0.1:${port}`,
      command: `node ${FAKE_DECOY} ${port}`,
      reuseExistingServer: false,
    })

    const teardown = await setup({
      projects: [
        { use: { decoyOptions: options(first) } },
        { use: { decoyOptions: options(first) } },
        { use: { decoyOptions: options(second) } },
        { use: {} },
      ],
    })
    expect(await isHealthy(`http://127.0.0.1:${first}`)).toBe(true)
    expect(await isHealthy(`http://127.0.0.1:${second}`)).toBe(true)

    await teardown()
    expect(await isHealthy(`http://127.0.0.1:${first}`)).toBe(false)
    expect(await isHealthy(`http://127.0.0.1:${second}`)).toBe(false)
  })

  test('refuses projects that share a url but start Decoy differently', async () => {
    const url = `http://127.0.0.1:${await freePort()}`
    await expect(
      setup({
        projects: [
          { use: { decoyOptions: { url, command: 'decoy serve --config a.yaml' } } },
          { use: { decoyOptions: { url, command: 'decoy serve --config b.yaml' } } },
        ],
      }),
    ).rejects.toThrow(/share the Decoy at .* but set different command/)
  })
})
