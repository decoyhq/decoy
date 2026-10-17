import { randomUUID } from 'node:crypto'
import type {
  Fixtures,
  PlaywrightTestArgs,
  PlaywrightTestOptions,
  PlaywrightWorkerArgs,
  PlaywrightWorkerOptions,
  TestInfo,
} from '@playwright/test'
import { type DecoyOptions, isHealthy, resolveOptions } from './options'
import { routeToDecoy } from './routing'

/** The per-test handle to one Decoy Session. */
export interface DecoyRouter {
  /** Session id stamped on this test's routed requests as `x-mock-session`. */
  readonly sessionId: string
}

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
 * import { createDecoyFixtures, type DecoyFixtures } from '@decoy/playwright'
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
      await routeToDecoy(
        context,
        (url, init) => context.request.fetch(url, init),
        decoyOptions,
        sessionIdFor(testInfo),
      )
      await use(context)
    },
    decoy: ({ decoyOptions }, use, testInfo) => provideDecoy(decoyOptions, use, testInfo),
  }
}

/** The `decoy` fixture body: validate config and reachability, then hand the test its Router. */
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
  const { url } = resolveOptions(decoyOptions)
  if (!(await isHealthy(url))) {
    throw new Error(
      `Decoy is not reachable at ${url}. Start it with the \`setup\` global setup or run \`decoy serve\`.`,
    )
  }
  await use({ sessionId: sessionIdFor(testInfo) })
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
