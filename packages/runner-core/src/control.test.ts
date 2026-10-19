import { createServer } from 'node:http'
import { afterEach, describe, expect, test } from '@rstest/core'
import { startFakeDecoy } from '../fixtures'
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

  test('useRoute takes one Behavior address `route:case:behavior`', async () => {
    const decoy = await fake()
    await createRouter(decoy.url, 's').useRoute('get-user:user-123:broken')

    expect(decoy.calls).toEqual([
      {
        path: '/__decoy__/control/useRoute',
        session: 's',
        body: '{"route":"get-user","case":"user-123","behavior":"broken"}',
      },
    ])
  })

  test('useRoute rejects a malformed Behavior address without calling Decoy', async () => {
    const decoy = await fake()
    const router = createRouter(decoy.url, 's')

    for (const address of ['get-user:user-123', 'get-user::broken', 'a:b:c:d', '']) {
      await expect(router.useRoute(address)).rejects.toThrow(
        `Invalid Behavior address \`${address}\`: expected \`route:case:behavior\``,
      )
    }
    expect(decoy.calls).toEqual([])
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

  test('gives up on a Decoy that never answers, naming the verb and url', async () => {
    const server = createServer(() => undefined)
    await new Promise<void>((done) => server.listen(0, '127.0.0.1', done))
    closers.push(async () => {
      server.closeAllConnections()
      await new Promise((done) => server.close(done))
    })
    const address = server.address()
    const url = `http://127.0.0.1:${typeof address === 'object' && address ? address.port : 0}`

    await expect(createRouter(url, 's', 50).reset()).rejects.toThrow(
      `Decoy reset at ${url} failed:`,
    )
  })
})
