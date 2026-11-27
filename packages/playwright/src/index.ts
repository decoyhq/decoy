import { test as base } from '@playwright/test'
import { createDecoyFixtures } from './fixtures'
import type { DecoyFixtures } from './fixtures'

export type { DecoyConnection, DecoyRouter } from '@decoyhq/runner-core'
export { createDecoyFixtures, type DecoyFixtures, type DecoyTestOptions } from './fixtures'
export type { DecoyOptions, UrlPatterns } from './options'
export { type SetupConfig, setup, type Teardown } from './setup'
export { expect } from '@playwright/test'

/** Playwright's `test` with the Decoy fixtures merged in; `test.extend` it further as usual. */
export const test = base.extend<DecoyFixtures>(createDecoyFixtures())
