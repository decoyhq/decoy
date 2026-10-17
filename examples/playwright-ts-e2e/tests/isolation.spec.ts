import { expect, test } from './fixtures'

// Two tests, two browser contexts, one Decoy runtime. Serial order makes the leak check
// deterministic: if Sessions were shared, the second test would inherit `no-users`.
test.describe.configure({ mode: 'serial' })

let firstSessionId: string | undefined

test('one context switches its own Session', async ({ page, decoy }) => {
  firstSessionId = decoy.sessionId
  await decoy.useCollection('no-users')

  await page.goto('/')
  await page.getByRole('button', { name: 'Load user' }).click()
  await expect(page.getByRole('status')).toHaveText('Error 404: user not found')
})

test('another context keeps its own Selection', async ({ page, decoy }) => {
  expect(firstSessionId).toBeDefined()
  expect(decoy.sessionId).not.toBe(firstSessionId)

  await page.goto('/')
  await page.getByRole('button', { name: 'Load user' }).click()
  await expect(page.getByRole('status')).toHaveText('Ada Lovelace')
})
