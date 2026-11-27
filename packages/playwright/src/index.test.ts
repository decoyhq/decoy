import { expect as playwrightExpect } from '@playwright/test'
import type { TestType } from '@playwright/test'
import { expect, test } from '@rstest/core'
import { expectTypeOf } from 'expect-type'
import type { DecoyRouter } from '@decoyhq/runner-core'
import { expect as decoyExpect, test as decoyTest } from './index'

test('exports a Playwright `test` already extended with the Decoy fixtures', () => {
  expect(typeof decoyTest.extend).toBe('function')
  type Args = typeof decoyTest extends TestType<infer T, object> ? T : never
  expectTypeOf<Args['decoy']>().toEqualTypeOf<DecoyRouter>()
  expectTypeOf<Args>().toHaveProperty('page')
})

test("re-exports Playwright's `expect`", () => {
  expect(decoyExpect).toBe(playwrightExpect)
})
