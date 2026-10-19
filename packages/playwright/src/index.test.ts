import { expect as playwrightExpect, type TestType } from '@playwright/test'
import { expect, test } from '@rstest/core'
import { expectTypeOf } from 'expect-type'
import type { DecoyRouter } from '@decoyhq/runner-core'
import * as decoy from './index'

test('exports a Playwright `test` already extended with the Decoy fixtures', () => {
  expect(typeof decoy.test.extend).toBe('function')
  type Args = typeof decoy.test extends TestType<infer T, object> ? T : never
  expectTypeOf<Args['decoy']>().toEqualTypeOf<DecoyRouter>()
  expectTypeOf<Args>().toHaveProperty('page')
})

test("re-exports Playwright's `expect`", () => {
  expect(decoy.expect).toBe(playwrightExpect)
})
