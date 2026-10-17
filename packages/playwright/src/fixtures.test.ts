import { createServer } from 'node:http'
import { describe, expect, test } from '@rstest/core'
import { type DecoyRouter, provideDecoy, sessionIdFor } from './fixtures'

const testInfo = { titlePath: ['users.spec.ts', 'Shows a User!'] }

describe('decoy fixture', () => {
  test('fails clearly when Decoy is not configured', async () => {
    await expect(provideDecoy(undefined, async () => {}, testInfo)).rejects.toThrow(
      /use\.decoyOptions/,
    )
  })

  test('fails clearly when Decoy is configured but not reachable', async () => {
    await expect(
      provideDecoy({ url: 'http://127.0.0.1:1' }, async () => {}, testInfo),
    ).rejects.toThrow(/not reachable at http:\/\/127\.0\.0\.1:1/)
  })

  test('provides a Session id when Decoy is healthy', async () => {
    const server = createServer((_req, res) => res.writeHead(200).end())
    await new Promise<void>((done) => server.listen(0, '127.0.0.1', done))
    const address = server.address()
    if (address === null || typeof address === 'string') {
      throw new Error('expected a TCP address')
    }

    let router: DecoyRouter | undefined
    try {
      await provideDecoy(
        { url: `http://127.0.0.1:${address.port}` },
        async (value) => {
          router = value
        },
        testInfo,
      )
    } finally {
      await new Promise((done) => server.close(done))
    }

    expect(router?.sessionId).toMatch(/^users-spec-ts-shows-a-user-[0-9a-f-]{36}$/)
  })
})

describe('sessionIdFor', () => {
  test('is unique per call', () => {
    expect(sessionIdFor(testInfo)).not.toBe(sessionIdFor(testInfo))
  })
})
