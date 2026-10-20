import { chmodSync, mkdirSync, mkdtempSync, writeFileSync } from 'node:fs'
import { createServer, type Server } from 'node:http'
import { tmpdir } from 'node:os'
import { join, relative } from 'node:path'
import { afterEach, describe, expect, test } from '@rstest/core'
import { FAKE_DECOY } from '../fixtures'
import { isHealthy } from './health'
import { resolveServerOptions, type ServerOptions, startDecoy } from './server'

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

const start = (options: ServerOptions) =>
  startDecoy({ ...resolveServerOptions(options, {}), cwd: options.cwd ?? process.cwd() })

const cleanups: Array<() => Promise<unknown>> = []
afterEach(async () => {
  for (const cleanup of cleanups.splice(0)) {
    await cleanup()
  }
})

describe('resolveServerOptions', () => {
  test('applies defaults matching native decoy serve', () => {
    expect(resolveServerOptions({}, {})).toEqual({
      url: 'http://127.0.0.1:8080',
      command: 'decoy serve',
      cwd: undefined,
      reuseExistingServer: true,
      timeout: 60_000,
    })
  })

  test('requires a fresh server in CI and trims trailing slashes', () => {
    const options = resolveServerOptions({ url: 'http://127.0.0.1:9000/' }, { CI: 'true' })
    expect(options.reuseExistingServer).toBe(false)
    expect(options.url).toBe('http://127.0.0.1:9000')
  })
})

describe('startDecoy', () => {
  test('reuses a healthy Decoy without running the command', async () => {
    const { url, server } = await runningDecoy()
    cleanups.push(() => new Promise((done) => server.close(done)))

    const teardown = await start({ url, command: 'exit 1', reuseExistingServer: true })
    await teardown()

    expect(await isHealthy(url)).toBe(true)
  })

  test('refuses to share a healthy Decoy when reuse is disabled', async () => {
    const { url, server } = await runningDecoy()
    cleanups.push(() => new Promise((done) => server.close(done)))

    await expect(start({ url, reuseExistingServer: false })).rejects.toThrow(/already serving/)
  })

  test('starts the command, waits for health, and teardown stops it', async () => {
    const port = await freePort()
    const url = `http://127.0.0.1:${port}`

    const teardown = await start({
      url,
      command: `node ${FAKE_DECOY} ${port}`,
      reuseExistingServer: false,
    })
    expect(await isHealthy(url)).toBe(true)

    await teardown()
    expect(await isHealthy(url)).toBe(false)
  })

  test('replaces a Decoy that a hard-killed run started instead of reusing it', async () => {
    const port = await freePort()
    const url = `http://127.0.0.1:${port}`
    const options = { url, command: `node ${FAKE_DECOY} ${port}`, reuseExistingServer: true }
    const servingPid = async () =>
      ((await (await fetch(`${url}/__decoy__/health`)).json()) as { pid: number }).pid

    // The first run never tears down, as when its runner is killed.
    await start(options)
    const orphan = await servingPid()

    const teardown = await start(options)
    expect(await servingPid()).not.toBe(orphan)

    await teardown()
    expect(await isHealthy(url)).toBe(false)
  })

  test.skipIf(process.platform === 'win32')(
    'finds commands in node_modules/.bin like an npm script',
    async () => {
      const dir = mkdtempSync(join(tmpdir(), 'decoy-setup-'))
      const bin = join(dir, 'node_modules', '.bin')
      mkdirSync(bin, { recursive: true })
      writeFileSync(join(bin, 'decoy-fake'), `#!/bin/sh\nexec node ${FAKE_DECOY} "$@"\n`)
      chmodSync(join(bin, 'decoy-fake'), 0o755)
      const port = await freePort()
      const url = `http://127.0.0.1:${port}`

      const teardown = await start({
        url,
        command: `decoy-fake ${port}`,
        // Relative, like a hand-written config; resolved against process.cwd().
        cwd: relative(process.cwd(), dir),
        reuseExistingServer: false,
      })
      expect(await isHealthy(url)).toBe(true)
      await teardown()
    },
  )

  test('fails clearly when the command exits before serving', async () => {
    const port = await freePort()
    await expect(start({ url: `http://127.0.0.1:${port}`, command: 'exit 3' })).rejects.toThrow(
      /exited before serving \(code 3\)/,
    )
  })

  test('fails clearly and stops the command when health never succeeds', async () => {
    const port = await freePort()
    const other = await freePort()
    await expect(
      start({
        url: `http://127.0.0.1:${port}`,
        command: `node ${FAKE_DECOY} ${other}`,
        timeout: 500,
      }),
    ).rejects.toThrow(/did not become healthy .* within 500ms/)
    expect(await isHealthy(`http://127.0.0.1:${other}`)).toBe(false)
  })
})
