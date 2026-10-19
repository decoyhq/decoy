import { describe, expect, test } from '@rstest/core'
import { SESSION_HEADER } from '@decoyhq/runner-core'
import { forwardToDecoy, type InterceptedRequest, toMatchers } from './routing'

function fakeRequest(url: string, headers: Record<string, string> = {}) {
  const result: { continued?: Parameters<InterceptedRequest['continue']>[0]; aborted?: string } = {}
  const request: InterceptedRequest = {
    url: () => url,
    headers: () => headers,
    continue: (overrides) => {
      result.continued = overrides ?? {}
      return Promise.resolve()
    },
    abort: (errorCode) => {
      result.aborted = errorCode ?? 'failed'
      return Promise.resolve()
    },
  }
  return { request, result }
}

const decoyUrl = 'http://127.0.0.1:8080'

describe('toMatchers', () => {
  test('globs: `*` stays in a segment, `**` crosses them', () => {
    const [api] = toMatchers('**/api/**')
    expect(api?.test('http://app.test/api/users/1?x=1')).toBe(true)
    expect(api?.test('http://app.test/apix/users')).toBe(false)
    const [one] = toMatchers('http://app.test/*/users')
    expect(one?.test('http://app.test/v1/users')).toBe(true)
    expect(one?.test('http://app.test/v1/v2/users')).toBe(false)
  })

  test('RegExps pass through', () => {
    expect(toMatchers([/users/])[0]?.test('http://app.test/users')).toBe(true)
  })
})

describe('forwardToDecoy', () => {
  test('rewrites a matching request to Decoy, keeping headers and stamping the Session', async () => {
    const { request, result } = fakeRequest('http://app.test/api/users/1?x=1', { accept: 'a/b' })
    await forwardToDecoy(request, decoyUrl, toMatchers('**/api/**'), { sessionId: 's-1' })
    expect(result.continued).toEqual({
      url: 'http://127.0.0.1:8080/api/users/1?x=1',
      headers: { accept: 'a/b', [SESSION_HEADER]: 's-1' },
    })
  })

  test('continues other requests untouched', async () => {
    const { request, result } = fakeRequest('http://app.test/index.html')
    await forwardToDecoy(request, decoyUrl, toMatchers('**/api/**'), { sessionId: 's-1' })
    expect(result.continued).toEqual({})
  })

  test('aborts a matching request outside a test instead of sending it anywhere', async () => {
    const { request, result } = fakeRequest('http://app.test/api/users/1')
    await forwardToDecoy(request, decoyUrl, toMatchers('**/api/**'), { sessionId: undefined })
    expect(result).toEqual({ aborted: 'failed' })
  })
})
