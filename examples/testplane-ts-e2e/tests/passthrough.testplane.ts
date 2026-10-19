import { it } from 'testplane'

it('a Passthrough Behavior forwards to the local upstream', async ({ browser }) => {
  await browser.url('/')
  await browser.$('#load-profile').click()

  await expect(browser.$('#result')).toHaveText('Upstream profile (GET /api/profile)')
})
