import { randomUUID } from 'node:crypto'
import type { Fixtures, TestInfo } from '@playwright/test'
import { type DecoyOptions, isHealthy, resolveOptions } from './options'

/** The per-test handle to one Decoy Session. */
export interface DecoyRouter {
  /** Session id stamped on this test's routed requests as `x-mock-session`. */
  readonly sessionId: string
}

export interface DecoyFixtures {
  /** Decoy config; `undefined` (the default) leaves Decoy disabled. */
  decoyOptions: DecoyOptions | undefined
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
 * Fixtures never start Decoy; use `setup` as global setup or start it yourself.
 */
export function createDecoyFixtures(): Fixtures<DecoyFixtures> {
  return {
    decoyOptions: [undefined, { option: true }],
    decoy: async ({ decoyOptions }, use, testInfo) => {
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
    },
  }
}

/** A readable, practically unique Session id: `<file>-<title>-<uuid>`. */
export function sessionIdFor(testInfo: Pick<TestInfo, 'titlePath'>): string {
  const readable = testInfo.titlePath
    .join('-')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 60)
  return readable ? `${readable}-${randomUUID()}` : randomUUID()
}
