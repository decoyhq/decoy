import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs'
import { rm } from 'node:fs/promises'
import { createServer, type Server } from 'node:http'
import { tmpdir } from 'node:os'
import { join, relative } from 'node:path'
import { setTimeout as sleep } from 'node:timers/promises'
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

/** The pid of a process that has exited. */
async function deadPid(): Promise<number> {
  const child = spawn(process.execPath, ['-e', ''])
  await once(child, 'exit')
  return child.pid ?? 0
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
    const pidFile = join(tmpdir(), `decoy-runner-${port}.json`)
    const recorded = JSON.parse(readFileSync(pidFile, 'utf8')) as { runner: number }
    writeFileSync(pidFile, JSON.stringify({ ...recorded, runner: await deadPid() }))

    const teardown = await start(options)
    expect(await servingPid()).not.toBe(orphan)

    await teardown()
    expect(await isHealthy(url)).toBe(false)
  })

  test('reuses, not replaces, a Decoy whose runner is still running', async () => {
    const port = await freePort()
    const url = `http://127.0.0.1:${port}`
    const options = { url, command: `node ${FAKE_DECOY} ${port}`, reuseExistingServer: true }
    const servingPid = async () =>
      ((await (await fetch(`${url}/__decoy__/health`)).json()) as { pid: number }).pid

    const teardown = await start(options)
    const first = await servingPid()
    // Another runner, still running, owns this Decoy.
    const runner = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'])
    cleanups.push(() => Promise.resolve(runner.kill()))
    const pidFile = join(tmpdir(), `decoy-runner-${port}.json`)
    const recorded = JSON.parse(readFileSync(pidFile, 'utf8')) as { runner: number }
    writeFileSync(pidFile, JSON.stringify({ ...recorded, runner: runner.pid }))

    await (
      await start(options)
    )()
    expect(await servingPid()).toBe(first)

    await teardown()
    expect(await isHealthy(url)).toBe(false)
  })

  test.skipIf(process.platform === 'win32')(
    'stops Decoy when the runner exits without tearing down',
    async () => {
      const port = await freePort()
      const url = `http://127.0.0.1:${port}`
      const options = { url, command: `node ${FAKE_DECOY} ${port}`, reuseExistingServer: false }
      // A runner that exits mid-run, as Testplane does on a second Ctrl+C: `process.exit`, no
      // teardown. jiti: the runner loads this package's TypeScript source.
      const runner = spawn(
        process.execPath,
        [
          '-e',
          `const { startDecoy, resolveServerOptions } = require('jiti').createJiti(${JSON.stringify(import.meta.filename)})('./server.ts')
          startDecoy({ ...resolveServerOptions(${JSON.stringify(options)}, {}), cwd: process.cwd() })
            .then(() => process.exit(130))`,
        ],
        { stdio: 'inherit' },
      )
      cleanups.push(async () => {
        await rm(join(tmpdir(), `decoy-runner-${port}.json`), { force: true })
        // A leaked one, if this test fails.
        await fetch(`${url}/__decoy__/health`)
          .then(async (res) => process.kill(((await res.json()) as { pid: number }).pid))
          .catch(() => undefined)
      })
      const [code] = (await once(runner, 'exit')) as [number | null]
      expect(code).toBe(130)

      await sleep(500)
      expect(await isHealthy(url)).toBe(false)
    },
  )

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
    expect(existsSync(join(tmpdir(), `decoy-runner-${port}.json`))).toBe(false)
  })

  test('stops the command when the pidfile cannot be written', async () => {
    const port = await freePort()
    const url = `http://127.0.0.1:${port}`
    // A directory where the pidfile goes: neither writable nor removable as a file, like another
    // user's pidfile in a shared /tmp.
    const pidFile = join(tmpdir(), `decoy-runner-${port}.json`)
    mkdirSync(pidFile)
    cleanups.push(() => rm(pidFile, { recursive: true }))

    await expect(start({ url, command: `node ${FAKE_DECOY} ${port}` })).rejects.toThrow(/EISDIR/)
    await sleep(1000)
    expect(await isHealthy(url)).toBe(false)
  })

  test("leaves a running runner's pidfile it could not overwrite", async () => {
    const port = await freePort()
    // Another live runner's pidfile, read-only to this start.
    const runner = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'])
    cleanups.push(() => Promise.resolve(runner.kill()))
    const pidFile = join(tmpdir(), `decoy-runner-${port}.json`)
    writeFileSync(pidFile, JSON.stringify({ pid: runner.pid, runner: runner.pid }))
    chmodSync(pidFile, 0o444)
    cleanups.push(() => rm(pidFile, { force: true }))

    await expect(
      start({ url: `http://127.0.0.1:${port}`, command: `node ${FAKE_DECOY} ${port}` }),
    ).rejects.toThrow(/EACCES|EPERM/)
    expect(existsSync(pidFile)).toBe(true)
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
