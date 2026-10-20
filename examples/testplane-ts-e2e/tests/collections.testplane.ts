import { decoy } from '@decoyhq/testplane'
import { it } from 'testplane'

it('switching the Collection changes what the page shows', async ({ browser }) => {
  await decoy(browser).useCollection('no-users')

  await browser.url('/')
  await browser.$('#load-user').click()

  await expect(browser.$('#result')).toHaveText('Error 404: user not found')
})
