import { afterEach, describe, expect, test } from '@rstest/core'
import { startFakeDecoy } from '../fixtures'
import { newSessionId, openSession } from './session'

describe('newSessionId', () => {
  test('slugs the title and keeps ids unique', () => {
    const id = newSessionId('Users > Loading a user!')
    expect(id).toMatch(/^users-loading-a-user-[0-9a-f-]{36}$/)
    expect(newSessionId('Users > Loading a user!')).not.toBe(id)
  })

  test('falls back to a bare uuid for a title with no letters or digits', () => {
    expect(newSessionId('>>>')).toMatch(/^[0-9a-f-]{36}$/)
  })
})

describe('openSession', () => {
  const stops: Array<() => Promise<void>> = []
  afterEach(async () => {
    await Promise.all(stops.splice(0).map((stop) => stop()))
  })

  test('applies the default Collection to the new Session', async () => {
    const decoy = await startFakeDecoy()
    stops.push(decoy.close)

    const router = await openSession(decoy.url, 's-1', 'happy-path', 'hint')

    expect(router.sessionId).toBe('s-1')
    expect(decoy.calls).toEqual([
      {
        path: '/__decoy__/control/useCollection',
        session: 's-1',
        body: '{"collection":"happy-path"}',
      },
    ])
  })

  test('fails with the runner hint when Decoy is not reachable', async () => {
    await expect(openSession('http://127.0.0.1:9', 's-1', undefined, 'Start it.')).rejects.toThrow(
      'Decoy is not reachable at http://127.0.0.1:9. Start it.',
    )
  })
})
