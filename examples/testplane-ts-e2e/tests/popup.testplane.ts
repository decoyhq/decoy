import { it } from 'testplane'

it('a popup that fetches on load is routed from its first request', async ({ browser }) => {
  await browser.url('/')
  await browser.$('#open-popup').click()
  await browser.waitUntil(async () => (await browser.getWindowHandles()).length === 2)
  const [opener, popup] = await browser.getWindowHandles()
  await browser.switchToWindow(popup ?? '')

  await expect(browser.$('#result')).toHaveText('Ada Lovelace')

  await browser.closeWindow()
  await browser.switchToWindow(opener ?? '')
})
