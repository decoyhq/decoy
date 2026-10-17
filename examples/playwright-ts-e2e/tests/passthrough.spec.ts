import { expect, test } from './fixtures'

test('a Passthrough Behavior forwards to the local upstream', async ({ page }) => {
  await page.goto('/')
  await page.getByRole('button', { name: 'Load profile' }).click()

  await expect(page.getByRole('status')).toHaveText('Upstream profile (GET /api/profile)')
})
