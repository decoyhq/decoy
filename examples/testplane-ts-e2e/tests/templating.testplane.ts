import { it } from 'testplane'

it('a templated Behavior builds the response from the request', async ({ browser }) => {
  await browser.url('/')
  await browser.$('#load-user-2').click()

  await expect(browser.$('#result')).toHaveText('User 2')
})
