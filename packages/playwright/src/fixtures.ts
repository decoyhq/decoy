import { randomUUID } from 'node:crypto'
import type {
  Fixtures,
  PlaywrightTestArgs,
  PlaywrightTestOptions,
  PlaywrightWorkerArgs,
  PlaywrightWorkerOptions,
  TestInfo,
} from '@playwright/test'
import { createRouter, type DecoyRouter } from '@decoyhq/runner-core'
import { type DecoyOptions, isHealthy, resolveOptions } from './options'
import { fetchWithoutCookies, routeToDecoy } from './routing'

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
 * under the test's Session, whether or not the test asks for `decoy`. Fixtures never start Decoy;
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
      if (decoyOptions?.urls !== undefined) {
        const { url } = resolveOptions(decoyOptions)
        const router = await decoySession(decoyOptions, testInfo)
        await routeToDecoy(context, fetchWithoutCookies, url, decoyOptions.urls, router.sessionId)
      }
      await use(context)
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

async function startSession(decoyOptions: DecoyOptions, sessionId: string): Promise<DecoyRouter> {
  const { url } = resolveOptions(decoyOptions)
  if (!(await isHealthy(url))) {
    throw new Error(
      `Decoy is not reachable at ${url}. Start it with the \`setup\` global setup or run \`decoy serve\`.`,
    )
  }
  const router = createRouter(url, sessionId)
  if (decoyOptions.collection !== undefined) {
    await router.useCollection(decoyOptions.collection)
  }
  return router
}

const sessionIds = new WeakMap<object, string>()

/**
 * The test's Session id: readable `<file>-<title>-<uuid>`, created once per test so routing and the
 * `decoy` fixture agree.
 */
export function sessionIdFor(testInfo: Pick<TestInfo, 'titlePath'>): string {
  let sessionId = sessionIds.get(testInfo)
  if (sessionId === undefined) {
    sessionId = newSessionId(testInfo)
    sessionIds.set(testInfo, sessionId)
  }
  return sessionId
}

function newSessionId(testInfo: Pick<TestInfo, 'titlePath'>): string {
  const readable = testInfo.titlePath
    .join('-')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 60)
  return readable ? `${readable}-${randomUUID()}` : randomUUID()
}
