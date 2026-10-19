import { decoy } from '@decoyhq/testplane'
import { it } from 'testplane'

it('overriding one route changes it until reset', async ({ browser }) => {
  await browser.url('/')
  const loadUser = browser.$('#load-user')
  const result = browser.$('#result')

  await decoy(browser).useRoute('users-by-id', 'user-1', 'grace')
  await loadUser.click()
  await expect(result).toHaveText('Grace Hopper')

  await decoy(browser).reset()
  await loadUser.click()
  await expect(result).toHaveText('Ada Lovelace')
})

it('an unmapped API call fails closed with a Decoy miss', async ({ browser }) => {
  await browser.url('/')
  await browser.$('#load-orders').click()

  await expect(browser.$('#result')).toHaveText(
    'Error 501 (Decoy miss): no active route case matched request',
  )
})
