import { it } from 'testplane'

it("a fetch redirected by Decoy follows a relative Location on the page's origin", async ({
  browser,
}) => {
  await browser.url('/')
  await browser.$('#load-redirected').click()

  await expect(browser.$('#result')).toHaveText('User 2')
})

it("a navigation redirected by Decoy stays on the page's origin", async ({ browser }) => {
  await browser.url('/api/redirect')

  await expect(browser).toHaveUrl('http://127.0.0.1:4173/api/users/2')
  await expect(browser.$('body')).toHaveText(/User 2/)
})
