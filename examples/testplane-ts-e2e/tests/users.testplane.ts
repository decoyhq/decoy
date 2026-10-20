import { it } from 'testplane'

it('loading a user shows mocked data from Decoy', async ({ browser }) => {
  await browser.url('/')
  await browser.$('#load-user').click()
  await expect(browser.$('#result')).toHaveText('Ada Lovelace')
})
