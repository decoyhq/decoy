import { dirname } from 'node:path'
import {
  type DecoyRouter,
  newSessionId,
  openSession,
  resolveServerOptions,
  startDecoy,
  type Teardown,
} from '@decoyhq/runner-core'
import type Testplane from 'testplane'
import type { TestFunctionCtx } from 'testplane'
import type { DecoyOptions } from './options'
import { type CdpSession, routeToDecoy, type SessionHolder } from './routing'

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
        // First, ahead of the file's own top-level hooks (added when the file was read), so they
        // already run inside the Session. `afterEach` hooks run child-first, so appending ends
        // the Session after every user hook.
        const hooks = root.beforeEachHooks
        hooks.unshift(...hooks.splice(-1))
        root.afterEach(async function (this: TestFunctionCtx) {
          await endSession(this.browser)
        })
      })
    })
    return
  }

  let teardown: Teardown | undefined
  testplane.on(testplane.events.RUNNER_START, async () => {
    const resolved = resolveServerOptions(options)
    // Like Playwright: relative to the config file, so `--config apps/web/...` works from anywhere.
    const configDir = testplane.config.configPath ? dirname(testplane.config.configPath) : undefined
    teardown = await startDecoy({ ...resolved, cwd: resolved.cwd ?? configDir ?? process.cwd() })
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
  const { url } = resolveServerOptions(options)
  let state = states.get(browser.sessionId)
  if (state === undefined) {
    state = { holder: { sessionId: undefined }, routing: undefined, router: undefined }
    states.set(browser.sessionId, state)
  }
  // Stored before routing is set up, so `afterEach` ends this Session even if that step fails.
  state.router = await openSession(
    url,
    sessionId,
    options.collection,
    'Is the @decoyhq/testplane plugin enabled?',
  )
  if (options.urls !== undefined) {
    const { urls } = options
    // A failed setup isn't cached, so the next test in this browser tries again.
    state.routing ??= cdpOf(browser)
      .then((root) => routeToDecoy(root, url, urls, state.holder))
      .catch((error: unknown) => {
        state.routing = undefined
        throw error
      })
    await state.routing
  }
  state.holder.sessionId = sessionId
}

/**
 * Routing rewrites requests over CDP, so it needs Chrome or Chromium, in either WebDriver Classic
 * or BiDi sessions. WebDriver BiDi's own `network.continueRequest({ url })` can't replace it yet:
 * Chrome drops cookies on the rewritten request and Firefox aborts it (docs/design/testplane.md).
 */
async function cdpOf(browser: WebdriverIO.Browser): Promise<CdpSession> {
  let puppeteer: Awaited<ReturnType<WebdriverIO.Browser['getPuppeteer']>>
  try {
    puppeteer = await browser.getPuppeteer()
  } catch (error) {
    const name = browser.capabilities.browserName ?? 'this browser'
    // Chromium-based (Chrome, Chromium, Edge): CDP is there, so the connection itself failed.
    const message = /chrom|edge/i.test(name)
      ? `@decoyhq/testplane could not connect to ${name} over CDP to route \`urls\`: ${String(error)}`
      : `@decoyhq/testplane routes \`urls\` over CDP, which needs Chrome or Chromium; ${name} is not supported yet.`
    throw new Error(message, { cause: error })
  }
  return (await puppeteer.target().createCDPSession()) as unknown as CdpSession
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
  return newSessionId(`${ctx.currentTest.browserId} ${ctx.currentTest.fullTitle()}`)
}
