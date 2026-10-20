import { decoy } from '@decoyhq/testplane'
import { beforeEach, it } from 'testplane'

// A file's top-level hook already runs inside the test's Session, with routing on.
beforeEach(async ({ browser }) => {
  await decoy(browser).useCollection('no-users')
  await browser.url('/')
  await browser.$('#load-user').click()
})

it('a top-level beforeEach already runs inside the Session', async ({ browser }) => {
  await expect(browser.$('#result')).toHaveText('Error 404: user not found')
})
