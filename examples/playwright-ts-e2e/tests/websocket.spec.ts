import { expect, test } from './fixtures'

test('the page gets the on: connect message when its socket opens', async ({ page }) => {
  await page.goto('/live.html')
  await expect(page.getByRole('status')).toHaveText('Connected')
})

test('Decoy replies to a message the page sends', async ({ page }) => {
  await page.goto('/live.html')
  await expect(page.getByRole('status')).toHaveText('Connected')
  await page.getByRole('button', { name: 'Ask status' }).click()
  await expect(page.getByRole('status')).toHaveText('Friend: online')
})

test('useRoute changes the reply on an already open socket', async ({ page, decoy }) => {
  await page.goto('/live.html')
  await expect(page.getByRole('status')).toHaveText('Connected')
  await decoy.useRoute('friend-socket:status:offline')
  await page.getByRole('button', { name: 'Ask status' }).click()
  await expect(page.getByRole('status')).toHaveText('Friend: offline')
})

test('a reconnected page gets what the test sends next', async ({ page, decoy }) => {
  await page.goto('/live.html')
  await expect(page.getByRole('status')).toHaveText('Connected')
  await page.reload() // a new socket, so a new connection
  // The welcome comes once Decoy has the new socket, so `send` can't reach only the old one.
  await expect(page.getByRole('status')).toHaveText('Connected')
  const live = await decoy.connection('friend-socket')
  await live.send({ type: 'notice', text: 'after reconnect' })
  await expect(page.getByRole('list', { name: 'Notices' }).getByRole('listitem')).toHaveText([
    'after reconnect',
  ])
})

test.describe('useRoute and send reach only their own test', () => {
  // Both tests hold their socket open at the same time, each in its own Session.
  // shortcut: isolation is only exercised when Playwright runs both at once (>= 2 workers).
  test.describe.configure({ mode: 'parallel' })

  for (const [name, status] of [
    ['first', 'online'],
    ['second', 'offline'],
  ] as const) {
    test(`${name} test`, async ({ page, decoy }) => {
      await page.goto('/live.html')
      const live = await decoy.connection('friend-socket', { timeout: 5000 })
      if (status === 'offline') {
        await decoy.useRoute('friend-socket:status:offline')
      }
      await live.send({ type: 'notice', text: `for the ${name} test` })

      const notices = page.getByRole('list', { name: 'Notices' }).getByRole('listitem')
      await expect(notices).toHaveText([`for the ${name} test`])
      // Keep the socket open while the other test sends: its notice must never show up here.
      await page.waitForTimeout(1000)
      await expect(notices).toHaveText([`for the ${name} test`])
      // Run together, the other test's override is in by now; only this test's own one applies.
      await page.getByRole('button', { name: 'Ask status' }).click()
      await expect(page.getByRole('status')).toHaveText(`Friend: ${status}`)
    })
  }
})
