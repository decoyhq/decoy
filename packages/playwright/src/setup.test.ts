import { createServer, type Server } from 'node:http'
import { resolve } from 'node:path'
import { defineConfig, type FullConfig } from '@playwright/test'
import { afterEach, describe, expect, test } from '@rstest/core'
import type { DecoyTestOptions } from './fixtures'
import { type DecoyOptions, isHealthy, resolveOptions } from './options'
import { type SetupConfig, setup, type Teardown } from './setup'

const FAKE_DECOY = resolve(process.cwd(), 'fixtures/fake-decoy.mjs')

describe('config typing', () => {
  test('a typed Playwright config fits setup (checked by tsc)', () => {
    const fits = (config: FullConfig<DecoyTestOptions>): SetupConfig => config
    expect(fits).toBeTypeOf('function')
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

async function runningDecoy(): Promise<{ url: string; server: Server }> {
  const server = createServer((_req, res) => {
    res.writeHead(200).end('{"status":"ok"}')
  })
  const port = await listen(server)
  return { url: `http://127.0.0.1:${port}`, server }
}

const cleanups: Array<() => Promise<unknown>> = []
afterEach(async () => {
  for (const cleanup of cleanups.splice(0)) {
    await cleanup()
  }
})

describe('resolveOptions', () => {
  test('applies defaults matching native decoy serve', () => {
    expect(resolveOptions({}, {})).toEqual({
      url: 'http://127.0.0.1:8080',
      command: 'decoy serve',
      cwd: undefined,
      reuseExistingServer: true,
      timeout: 60_000,
    })
  })

  test('requires a fresh server in CI and trims trailing slashes', () => {
    const options = resolveOptions({ url: 'http://127.0.0.1:9000/' }, { CI: 'true' })
    expect(options.reuseExistingServer).toBe(false)
    expect(options.url).toBe('http://127.0.0.1:9000')
  })
})

describe('setup', () => {
  test('does nothing when Decoy is not configured', async () => {
    const teardown = await setup(config(undefined))
    await teardown()
  })

  test('reuses a healthy Decoy without running the command', async () => {
    const { url, server } = await runningDecoy()
    cleanups.push(() => new Promise((done) => server.close(done)))

    const teardown = await setup(config({ url, command: 'exit 1', reuseExistingServer: true }))
    await teardown()

    expect(await isHealthy(url)).toBe(true)
  })

  test('refuses to share a healthy Decoy when reuse is disabled', async () => {
    const { url, server } = await runningDecoy()
    cleanups.push(() => new Promise((done) => server.close(done)))

    await expect(setup(config({ url, reuseExistingServer: false }))).rejects.toThrow(
      /already serving/,
    )
  })

  test('starts the command, waits for health, and teardown stops it', async () => {
    const port = await freePort()
    const url = `http://127.0.0.1:${port}`

    const teardown: Teardown = await setup(
      config({ url, command: `node ${FAKE_DECOY} ${port}`, reuseExistingServer: false }),
    )
    expect(await isHealthy(url)).toBe(true)

    await teardown()
    expect(await isHealthy(url)).toBe(false)
  })

  test('fails clearly when the command exits before serving', async () => {
    const port = await freePort()
    await expect(
      setup(config({ url: `http://127.0.0.1:${port}`, command: 'exit 3' })),
    ).rejects.toThrow(/exited before serving \(code 3\)/)
  })

  test('fails clearly and stops the command when health never succeeds', async () => {
    const port = await freePort()
    const other = await freePort()
    await expect(
      setup(
        config({
          url: `http://127.0.0.1:${port}`,
          command: `node ${FAKE_DECOY} ${other}`,
          timeout: 500,
        }),
      ),
    ).rejects.toThrow(/did not become healthy .* within 500ms/)
    expect(await isHealthy(`http://127.0.0.1:${other}`)).toBe(false)
  })
})
