import { copyFileSync, mkdtempSync } from 'node:fs'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { isHealthy } from '@decoyhq/runner-core'
import { FAKE_DECOY, startFakeDecoy } from '@decoyhq/runner-core/testing'
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

type Hook = (this: unknown) => Promise<void>

/** The worker-process slice of Testplane the plugin uses: one root suite, read with `hooks`. */
function fakeWorker(hooks: Hook[]) {
  const afterEachHooks: Hook[] = []
  const root = {
    beforeEach: (hook: Hook) => hooks.push(hook),
    afterEach: (hook: Hook) => afterEachHooks.push(hook),
    get beforeEachHooks() {
      return hooks
    },
  }
  let testsRead: (collection: unknown) => void = () => undefined
  const testplane = {
    isWorker: () => true,
    events: { AFTER_TESTS_READ: 'afterTestsRead' },
    on: (_event: string, handler: typeof testsRead) => {
      testsRead = handler
    },
  }
  return {
    testplane: testplane as unknown as Testplane,
    readTests: () => {
      testsRead({
        eachRootSuite: (each: (suite: typeof root) => void) => {
          each(root)
        },
      })
    },
    afterEachHooks,
  }
}

describe('decoyPlugin (worker)', () => {
  test("its beforeEach runs ahead of the file's own top-level hooks", () => {
    const own: Hook = () => Promise.resolve()
    const hooks = [own]
    const { testplane, readTests } = fakeWorker(hooks)

    decoyPlugin(testplane, {})
    readTests()

    expect(hooks).toHaveLength(2)
    expect(hooks[1]).toBe(own)
  })

  test('ends the Session even when routing setup fails', async () => {
    const decoy = await startFakeDecoy()
    const hooks: Hook[] = []
    const { testplane, readTests, afterEachHooks } = fakeWorker(hooks)
    decoyPlugin(testplane, { url: decoy.url, urls: '**/api/**' })
    readTests()
    const ctx = {
      browser: {
        sessionId: 'wd-1',
        capabilities: { browserName: 'firefox' },
        getPuppeteer: () => Promise.reject(new Error('no CDP')),
      },
      currentTest: { browserId: 'firefox', fullTitle: () => 'a test' },
    }

    await expect(hooks[0]?.call(ctx)).rejects.toThrow(/needs Chrome or Chromium/)
    await afterEachHooks[0]?.call(ctx)

    expect(decoy.calls.map((call) => call.path)).toEqual(['/__decoy__/control/endSession'])
    await decoy.close()
  })

  test('a failed routing setup is retried by the next test in that browser', async () => {
    const decoy = await startFakeDecoy()
    const hooks: Hook[] = []
    const { testplane, readTests, afterEachHooks } = fakeWorker(hooks)
    decoyPlugin(testplane, { url: decoy.url, urls: '**/api/**' })
    readTests()
    let attempts = 0
    const ctx = {
      browser: {
        sessionId: 'wd-2',
        capabilities: { browserName: 'chrome' },
        getPuppeteer: () => {
          attempts++
          return Promise.reject(new Error('CDP not ready'))
        },
      },
      currentTest: { browserId: 'chrome', fullTitle: () => 'a test' },
    }

    for (let test = 0; test < 2; test++) {
      await expect(hooks[0]?.call(ctx)).rejects.toThrow(
        /could not connect to chrome over CDP .*CDP not ready/,
      )
      await afterEachHooks[0]?.call(ctx)
    }

    expect(attempts).toBe(2)
    await decoy.close()
  })
})
