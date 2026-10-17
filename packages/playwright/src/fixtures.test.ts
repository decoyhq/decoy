import { createServer } from 'node:http'
import type { AddressInfo } from 'node:net'
import type { TestInfo } from '@playwright/test'
import { describe, expect, test } from '@rstest/core'
import { createDecoyFixtures, type DecoyRouter, sessionIdFor } from './fixtures'
import type { DecoyOptions } from './options'

type DecoyFixture = (
  args: { decoyOptions: DecoyOptions | undefined },
  use: (router: DecoyRouter) => Promise<void>,
  testInfo: TestInfo,
) => Promise<void>

const decoyFixture = createDecoyFixtures().decoy as unknown as DecoyFixture
const testInfo = { titlePath: ['users.spec.ts', 'Shows a User!'] } as TestInfo

describe('decoy fixture', () => {
  test('fails clearly when Decoy is not configured', async () => {
    await expect(
      decoyFixture({ decoyOptions: undefined }, async () => {}, testInfo),
    ).rejects.toThrow(/use\.decoyOptions/)
  })

  test('fails clearly when Decoy is configured but not reachable', async () => {
    await expect(
      decoyFixture({ decoyOptions: { url: 'http://127.0.0.1:1' } }, async () => {}, testInfo),
    ).rejects.toThrow(/not reachable at http:\/\/127\.0\.0\.1:1/)
  })

  test('provides a Session id when Decoy is healthy', async () => {
    const server = createServer((_req, res) => res.writeHead(200).end())
    await new Promise<void>((done) => server.listen(0, '127.0.0.1', done))
    const { port } = server.address() as AddressInfo

    let router: DecoyRouter | undefined
    try {
      await decoyFixture(
        { decoyOptions: { url: `http://127.0.0.1:${port}` } },
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
