import { createDecoyFixtures, type DecoyFixtures } from '@decoyhq/playwright'
import { test as base } from '@playwright/test'

export const test = base.extend<DecoyFixtures>(createDecoyFixtures())
export { expect } from '@playwright/test'
