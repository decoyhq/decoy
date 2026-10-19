import { randomUUID } from 'node:crypto'
import { createRouter, type DecoyRouter, startDecoy, type Teardown } from '@decoyhq/runner-core'
import type Testplane from 'testplane'
import type { TestFunctionCtx } from 'testplane'
import { type DecoyOptions, isHealthy, resolveOptions } from './options'
import { type InterceptableBrowser, routeToDecoy, type SessionHolder } from './routing'

/** Per-browser state: Testplane reuses one browser for many tests, one test at a time. */
interface BrowserState {
  holder: SessionHolder
  routing: Promise<void> | undefined
  router: DecoyRouter | undefined
}

/**
 * Keyed by WebDriver session id: Testplane hands each test a fresh `browser` object for the same
 * session. Shared through `globalThis`: Testplane `require`s the plugin (CJS build) while tests
 * `import` `decoy` (ESM build), so module-level state would exist twice.
 */
const registry = Symbol.for('@decoyhq/testplane/states')
const states: Map<string, BrowserState> = ((
  globalThis as { [registry]?: Map<string, BrowserState> }
)[registry] ??= new Map<string, BrowserState>())

/**
 * Testplane plugin. In the master process it starts (or reuses) Decoy for the run; in each worker
 * it gives every test its own Session, applies `collection`, and, with `urls`, routes matching
 * browser requests to Decoy. Chromium only: routing goes through CDP (`browser.getPuppeteer()`).
 *
 * ```ts
 * // .testplane.conf.ts
 * plugins: { '@decoyhq/testplane': { urls: '**' + '/api/**', collection: 'happy-path' } }
 * ```
 */
export default function decoyPlugin(testplane: Testplane, options: DecoyOptions = {}): void {
  if (options.enabled === false) {
    return
  }
  if (testplane.isWorker()) {
    testplane.on(testplane.events.AFTER_TESTS_READ, (collection) => {
      collection.eachRootSuite((root) => {
        root.beforeEach(async function (this: TestFunctionCtx) {
          await beginSession(this.browser, options, sessionIdFor(this))
        })
        root.afterEach(async function (this: TestFunctionCtx) {
          await endSession(this.browser)
        })
      })
    })
    return
  }

  let teardown: Teardown | undefined
  testplane.on(testplane.events.RUNNER_START, async () => {
    const resolved = resolveOptions(options)
    teardown = await startDecoy({ ...resolved, cwd: resolved.cwd ?? process.cwd() })
  })
  testplane.on(testplane.events.RUNNER_END, async () => {
    await teardown?.()
  })
}

/** This test's Router. Throws outside a test, or when the plugin is off. */
export function decoy(browser: WebdriverIO.Browser): DecoyRouter {
  const router = states.get(browser.sessionId)?.router
  if (router === undefined) {
    throw new Error(
      '`decoy(browser)` needs the @decoyhq/testplane plugin enabled and must be called inside a test.',
    )
  }
  return router
}

async function beginSession(
  browser: WebdriverIO.Browser,
  options: DecoyOptions,
  sessionId: string,
): Promise<void> {
  const { url } = resolveOptions(options)
  let state = states.get(browser.sessionId)
  if (state === undefined) {
    state = { holder: { sessionId: undefined }, routing: undefined, router: undefined }
    states.set(browser.sessionId, state)
  }
  if (!(await isHealthy(url))) {
    throw new Error(`Decoy is not reachable at ${url}. Is the @decoyhq/testplane plugin enabled?`)
  }
  const router = createRouter(url, sessionId)
  if (options.collection !== undefined) {
    await router.useCollection(options.collection)
  }
  if (options.urls !== undefined) {
    const { urls } = options
    state.routing ??= browser
      .getPuppeteer()
      .then((puppeteer) =>
        routeToDecoy(puppeteer as unknown as InterceptableBrowser, url, urls, state.holder),
      )
    await state.routing
  }
  state.router = router
  state.holder.sessionId = sessionId
}

/** Best effort: a Decoy that is already gone can't hold the Session either. */
async function endSession(browser: WebdriverIO.Browser): Promise<void> {
  const state = states.get(browser.sessionId)
  const router = state?.router
  if (state === undefined || router === undefined) {
    return
  }
  state.router = undefined
  state.holder.sessionId = undefined
  await router.endSession().catch(() => undefined)
}

/** Readable `<browser>-<full title>-<uuid>`, so Decoy's logs point at the test. */
function sessionIdFor(ctx: TestFunctionCtx): string {
  const readable = `${ctx.currentTest.browserId} ${ctx.currentTest.fullTitle()}`
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 60)
  return readable ? `${readable}-${randomUUID()}` : randomUUID()
}
