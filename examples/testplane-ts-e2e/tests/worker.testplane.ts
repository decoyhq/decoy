import { it } from 'testplane'

for (const [kind, button] of [
  ['worker', '#load-user-in-worker'],
  ['shared worker', '#load-user-in-shared-worker'],
  ['service worker', '#load-user-in-service-worker'],
]) {
  it(`a ${kind}'s own fetch is routed to Decoy`, async ({ browser }) => {
    await browser.url('/')
    await browser.$(button).click()

    await expect(browser.$('#result')).toHaveText('Ada Lovelace')
  })
}
