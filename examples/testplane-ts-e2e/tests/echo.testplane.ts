import { it } from 'testplane'

it('a routed request keeps its body, cookies, and headers', async ({ browser }) => {
  await browser.url('/')
  await browser.$('#send-echo').click()

  await expect(browser.$('#result')).toHaveText('Ada / abc / app')
})
