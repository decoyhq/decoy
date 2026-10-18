import type { test as base } from '@playwright/test'
import { describe, expect, test } from '@rstest/core'
import { expectTypeOf } from 'expect-type'
import { startFakeDecoy } from '../fixtures/fake-decoy-server'
import type { DecoyRouter } from './control'
import {
  createDecoyFixtures,
  type DecoyFixtures,
  decoySession,
  provideDecoy,
  sessionIdFor,
} from './fixtures'

const testInfo = { titlePath: ['users.spec.ts', 'Shows a User!'] }
const ignoreRouter = () => Promise.resolve()

test('fixtures compose onto Playwright base test', () => {
  expectTypeOf(createDecoyFixtures()).toExtend<Parameters<typeof base.extend<DecoyFixtures>>[0]>()
})

describe('decoy fixture', () => {
  test('fails clearly when Decoy is not configured', async () => {
    await expect(provideDecoy(undefined, ignoreRouter, testInfo)).rejects.toThrow(
      /use\.decoyOptions/,
    )
  })

  test('fails clearly when Decoy is configured but not reachable', async () => {
    await expect(
      provideDecoy({ url: 'http://127.0.0.1:1' }, ignoreRouter, { titlePath: ['down'] }),
    ).rejects.toThrow(/not reachable at http:\/\/127\.0\.0\.1:1/)
  })

  test('hands the test a Router for its Session', async () => {
    const decoy = await startFakeDecoy()
    let router: DecoyRouter | undefined
    try {
      await provideDecoy(
        { url: decoy.url },
        (value) => {
          router = value
          return Promise.resolve()
        },
        testInfo,
      )
    } finally {
      await decoy.close()
    }
    expect(router?.sessionId).toMatch(/^users-spec-ts-shows-a-user-[0-9a-f-]{36}$/)
    // The Session is freed in Decoy when the test ends.
    expect(decoy.calls).toEqual([
      { path: '/__decoy__/control/endSession', session: router?.sessionId, body: '' },
    ])
  })
})

describe('context fixture', () => {
  test('routes the context under the test Session and ends that Session afterwards', async () => {
    const decoy = await startFakeDecoy()
    const patterns: unknown[] = []
    const context = {
      route: (pattern: unknown) => {
        patterns.push(pattern)
        return Promise.resolve()
      },
      request: { fetch: () => Promise.reject(new Error('not routed in this test')) },
    }
    const info = { titlePath: ['context'] }
    const { context: contextFixture } = createDecoyFixtures() as unknown as {
      context: (
        args: { context: typeof context; decoyOptions: object },
        use: (value: unknown) => Promise<void>,
        testInfo: typeof info,
      ) => Promise<void>
    }
    try {
      await contextFixture(
        { context, decoyOptions: { url: decoy.url, urls: '**/api/**' } },
        () => Promise.resolve(),
        info,
      )
    } finally {
      await decoy.close()
    }
    expect(patterns).toEqual(['**/api/**'])
    expect(decoy.calls).toEqual([
      { path: '/__decoy__/control/endSession', session: sessionIdFor(info), body: '' },
    ])
  })
})

describe('decoySession', () => {
  test('applies the configured Collection once, before the Router is handed out', async () => {
    const decoy = await startFakeDecoy()
    const info = { titlePath: ['checkout'] }
    try {
      const first = await decoySession({ url: decoy.url, collection: 'happy-path' }, info)
      const second = await decoySession({ url: decoy.url, collection: 'happy-path' }, info)
      expect(second).toBe(first)
      expect(decoy.calls).toEqual([
        {
          path: '/__decoy__/control/useCollection',
          session: first.sessionId,
          body: '{"collection":"happy-path"}',
        },
      ])
    } finally {
      await decoy.close()
    }
  })

  test('fails test setup when the configured Collection is unknown', async () => {
    const decoy = await startFakeDecoy(() => ({
      status: 400,
      body: '{"error":"unknown collection `ghost`"}',
    }))
    try {
      await expect(
        decoySession({ url: decoy.url, collection: 'ghost' }, { titlePath: ['ghost'] }),
      ).rejects.toThrow('unknown collection `ghost`')
    } finally {
      await decoy.close()
    }
  })
})

describe('sessionIdFor', () => {
  test('is stable within a test and unique across tests', () => {
    const other = { titlePath: ['users.spec.ts', 'Shows a User!'] }
    expect(sessionIdFor(testInfo)).toBe(sessionIdFor(testInfo))
    expect(sessionIdFor(other)).not.toBe(sessionIdFor(testInfo))
  })
})
