import { decoy } from '@decoyhq/testplane'
import { describe, it } from 'testplane'

// Testplane runs these one after another in the same browser session (testsPerSession defaults to
// Infinity), so a leaked Selection would show up in the second test.
describe('Sessions', () => {
  let firstSessionId: string | undefined

  it('one test switches its own Session', async ({ browser }) => {
    firstSessionId = decoy(browser).sessionId
    await decoy(browser).useCollection('no-users')

    await browser.url('/')
    await browser.$('#load-user').click()
    await expect(browser.$('#result')).toHaveText('Error 404: user not found')
  })

  it('the next test in the same browser keeps its own Selection', async ({ browser }) => {
    expect(firstSessionId).toBeDefined()
    expect(decoy(browser).sessionId).not.toBe(firstSessionId)

    await browser.url('/')
    await browser.$('#load-user').click()
    await expect(browser.$('#result')).toHaveText('Ada Lovelace')
  })
})
