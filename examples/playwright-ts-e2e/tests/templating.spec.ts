import { expect, test } from './fixtures'

test('a templated Behavior builds the response from the request', async ({ page }) => {
  await page.goto('/')
  await page.getByRole('button', { name: 'Load second user' }).click()

  await expect(page.getByRole('status')).toHaveText('User 2')
})
