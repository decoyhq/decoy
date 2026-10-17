import { afterEach, describe, expect, test } from '@rstest/core'
import { startFakeDecoy } from '../fixtures/fake-decoy-server'
import { createRouter } from './control'

const closers: Array<() => Promise<void>> = []
afterEach(async () => {
  for (const close of closers.splice(0)) {
    await close()
  }
})

async function fake(...args: Parameters<typeof startFakeDecoy>) {
  const decoy = await startFakeDecoy(...args)
  closers.push(decoy.close)
  return decoy
}

describe('createRouter', () => {
  test('calls each Controller verb immediately for its own Session', async () => {
    const decoy = await fake()
    const router = createRouter(decoy.url, 'session-a')

    await router.useCollection('empty')
    await router.useRoute('get-user', 'user-123', 'broken')
    await router.reset()

    expect(decoy.calls).toEqual([
      {
        path: '/__decoy__/control/useCollection',
        session: 'session-a',
        body: '{"collection":"empty"}',
      },
      {
        path: '/__decoy__/control/useRoute',
        session: 'session-a',
        body: '{"route":"get-user","case":"user-123","behavior":"broken"}',
      },
      { path: '/__decoy__/control/reset', session: 'session-a', body: '' },
    ])
    expect(router.sessionId).toBe('session-a')
  })

  test('rejects at the call site with the Control API error', async () => {
    const decoy = await fake(() => ({
      status: 400,
      body: '{"error":"unknown collection `ghost`"}',
    }))
    const router = createRouter(decoy.url, 's')

    await expect(router.useCollection('ghost')).rejects.toThrow(
      'Decoy useCollection failed (400): unknown collection `ghost`',
    )
    await expect(router.useRoute('r', 'c', 'b')).rejects.toThrow(/useRoute failed \(400\)/)
    await expect(router.reset()).rejects.toThrow(/reset failed \(400\)/)
  })

  test('reports a non-envelope error body as-is', async () => {
    const decoy = await fake(() => ({ status: 500, body: 'boom' }))
    await expect(createRouter(decoy.url, 's').reset()).rejects.toThrow('reset failed (500): boom')
  })
})
