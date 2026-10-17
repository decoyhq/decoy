import type { test as base } from '@playwright/test'
import { describe, expect, test } from '@rstest/core'
import { startFakeDecoy } from '../fixtures/fake-decoy-server'
import type { DecoyRouter } from './control'
import {
  createDecoyFixtures,
  type DecoyFixtures,
  decoySession,
  provideDecoy,
  sessionIdFor,
} from './fixtures'

// Compile-time guard: users compose the fixtures onto Playwright's base test.
const compose = (playwrightTest: typeof base) =>
  playwrightTest.extend<DecoyFixtures>(createDecoyFixtures())
void compose

const testInfo = { titlePath: ['users.spec.ts', 'Shows a User!'] }

describe('decoy fixture', () => {
  test('fails clearly when Decoy is not configured', async () => {
    await expect(provideDecoy(undefined, async () => {}, testInfo)).rejects.toThrow(
      /use\.decoyOptions/,
    )
  })

  test('fails clearly when Decoy is configured but not reachable', async () => {
    await expect(
      provideDecoy({ url: 'http://127.0.0.1:1' }, async () => {}, { titlePath: ['down'] }),
    ).rejects.toThrow(/not reachable at http:\/\/127\.0\.0\.1:1/)
  })

  test('hands the test a Router for its Session', async () => {
    const decoy = await startFakeDecoy()
    let router: DecoyRouter | undefined
    try {
      await provideDecoy(
        { url: decoy.url },
        async (value) => {
          router = value
        },
        testInfo,
      )
    } finally {
      await decoy.close()
    }
    expect(router?.sessionId).toMatch(/^users-spec-ts-shows-a-user-[0-9a-f-]{36}$/)
    expect(decoy.calls).toEqual([])
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
