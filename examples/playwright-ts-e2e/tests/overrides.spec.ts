import { expect, test } from './fixtures'

test('overriding one route changes it until reset', async ({ page, decoy }) => {
  await page.goto('/')
  const loadUser = page.getByRole('button', { name: 'Load user' })
  const result = page.getByRole('status')

  await decoy.useRoute('users-by-id', 'user-1', 'grace')
  await loadUser.click()
  await expect(result).toHaveText('Grace Hopper')

  await decoy.reset()
  await loadUser.click()
  await expect(result).toHaveText('Ada Lovelace')
})

test('an unmapped API call fails closed with a Decoy miss', async ({ page }) => {
  await page.goto('/')
  await page.getByRole('button', { name: 'Load orders' }).click()

  await expect(page.getByRole('status')).toHaveText(
    'Error 501 (Decoy miss): no active route case matched request',
  )
})
