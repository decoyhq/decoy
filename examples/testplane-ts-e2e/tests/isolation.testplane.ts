import { decoy } from '@decoyhq/testplane'
import { describe, it } from 'testplane'

// Testplane reuses a browser for many tests (testsPerSession defaults to Infinity), so when these
// two share one, a leaked Selection would show up in the second. Each test stands alone: Testplane
// may hand them to different workers or browsers, where the second passes trivially.
describe('Sessions', () => {
  it('one test switches its own Session', async ({ browser }) => {
    await decoy(browser).useCollection('no-users')

    await browser.url('/')
    await browser.$('#load-user').click()
    await expect(browser.$('#result')).toHaveText('Error 404: user not found')
  })

  it('the next test in the same browser keeps its own Selection', async ({ browser }) => {
    await browser.url('/')
    await browser.$('#load-user').click()
    await expect(browser.$('#result')).toHaveText('Ada Lovelace')
  })
})
