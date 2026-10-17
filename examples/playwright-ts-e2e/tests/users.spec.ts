import { expect, test } from './fixtures'

test('loading a user shows mocked data from Decoy', async ({ page }) => {
  await page.goto('/')
  await page.getByRole('button', { name: 'Load user' }).click()
  await expect(page.getByRole('status')).toHaveText('Ada Lovelace')
})
