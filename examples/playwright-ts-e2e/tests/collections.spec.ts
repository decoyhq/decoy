import { expect, test } from './fixtures'

test('switching the Collection changes what the page shows', async ({ page, decoy }) => {
  await decoy.useCollection('no-users')

  await page.goto('/')
  await page.getByRole('button', { name: 'Load user' }).click()

  await expect(page.getByRole('status')).toHaveText('Error 404: user not found')
})
