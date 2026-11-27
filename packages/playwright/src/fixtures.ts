import type {
  Fixtures,
  PlaywrightTestArgs,
  PlaywrightTestOptions,
  PlaywrightWorkerArgs,
  PlaywrightWorkerOptions,
  TestInfo,
} from '@playwright/test'
import { newSessionId, openSession, resolveServerOptions } from '@decoyhq/runner-core'
import type { DecoyRouter } from '@decoyhq/runner-core'
import type { DecoyOptions } from './options'
import { fetchWithoutCookies, routeToDecoy } from './routing'
import { routeWebSocketsToDecoy } from './websocket'

/** Option fixtures, for typing config: `defineConfig<DecoyTestOptions>({ use: { decoyOptions } })`. */
export interface DecoyTestOptions {
  /** Decoy config; `undefined` (the default) leaves Decoy disabled. */
  decoyOptions: DecoyOptions | undefined
}

export interface DecoyFixtures extends DecoyTestOptions {
  decoy: DecoyRouter
}

/**
 * Fixtures to merge into your own with `test.extend`:
 *
 * ```ts
 * import { test as base } from '@playwright/test'
 * import { createDecoyFixtures, type DecoyFixtures } from '@decoyhq/playwright'
 *
 * export const test = base.extend<DecoyFixtures>(createDecoyFixtures())
 * ```
 *
 * When `decoyOptions.urls` is set, every test's browser context routes matching requests to Decoy
 * under the test's Session, whether or not the test asks for `decoy`; `decoyOptions.webSocketUrls`
 * does the same for page WebSockets. Fixtures never start Decoy;
 * use `setup` as global setup or start it yourself.
 */
export function createDecoyFixtures(): Fixtures<
  DecoyFixtures,
  object,
  PlaywrightTestArgs & PlaywrightTestOptions,
  PlaywrightWorkerArgs & PlaywrightWorkerOptions
> {
  return {
    decoyOptions: [undefined, { option: true }],
    context: async ({ context, decoyOptions }, use, testInfo) => {
      const { urls, webSocketUrls } = decoyOptions ?? {}
      let closeSockets: (() => void) | undefined
      if (decoyOptions && (urls !== undefined || webSocketUrls !== undefined)) {
        const { url } = resolveServerOptions(decoyOptions)
        const { sessionId } = await decoySession(decoyOptions, testInfo)
        if (urls !== undefined) {
          await routeToDecoy(context, fetchWithoutCookies, url, urls, sessionId)
        }
        if (webSocketUrls !== undefined) {
          closeSockets = await routeWebSocketsToDecoy(context, url, webSocketUrls, sessionId)
        }
      }
      await use(context)
      closeSockets?.()
      await endDecoySession(testInfo)
    },
    decoy: ({ decoyOptions }, use, testInfo) => provideDecoy(decoyOptions, use, testInfo),
  }
}

/** The `decoy` fixture body: fail clearly when Decoy is disabled, else hand over the Router. */
export async function provideDecoy(
  decoyOptions: DecoyOptions | undefined,
  use: (router: DecoyRouter) => Promise<void>,
  testInfo: Pick<TestInfo, 'titlePath'>,
): Promise<void> {
  if (!decoyOptions) {
    throw new Error(
      'The `decoy` fixture needs Decoy to be configured: set `use.decoyOptions` in your Playwright config.',
    )
  }
  await use(await decoySession(decoyOptions, testInfo))
  await endDecoySession(testInfo)
}

/**
 * Ends the test's Session in Decoy once the test is done, so a long-lived Decoy doesn't keep one
 * Selection per test. Best effort: a Decoy that is already gone can't hold the Session either.
 */
async function endDecoySession(testInfo: Pick<TestInfo, 'titlePath'>): Promise<void> {
  const session = sessions.get(testInfo)
  if (session === undefined) {
    return
  }
  sessions.delete(testInfo)
  const router = await session.catch(() => undefined)
  await router?.endSession().catch(() => undefined)
}

const sessions = new WeakMap<object, Promise<DecoyRouter>>()

/**
 * The test's Session, set up once: check Decoy is reachable, then apply the configured default
 * Collection, before any routed browser request or test code runs.
 */
export function decoySession(
  decoyOptions: DecoyOptions,
  testInfo: Pick<TestInfo, 'titlePath'>,
): Promise<DecoyRouter> {
  let session = sessions.get(testInfo)
  if (session === undefined) {
    session = startSession(decoyOptions, sessionIdFor(testInfo))
    sessions.set(testInfo, session)
  }
  return session
}

function startSession(decoyOptions: DecoyOptions, sessionId: string): Promise<DecoyRouter> {
  return openSession(
    resolveServerOptions(decoyOptions).url,
    sessionId,
    decoyOptions.collection,
    'Start it with the `setup` global setup or run `decoy serve`.',
  )
}

const sessionIds = new WeakMap<object, string>()

/**
 * The test's Session id: readable `<file>-<title>-<uuid>`, created once per test so routing and the
 * `decoy` fixture agree.
 */
export function sessionIdFor(testInfo: Pick<TestInfo, 'titlePath'>): string {
  let sessionId = sessionIds.get(testInfo)
  if (sessionId === undefined) {
    sessionId = newSessionId(testInfo.titlePath.join('-'))
    sessionIds.set(testInfo, sessionId)
  }
  return sessionId
}
