import { describe, expect, test } from '@rstest/core'
import {
  type DecoyFetch,
  type DecoyResponse,
  forwardToDecoy,
  type RoutableRoute,
  routeToDecoy,
} from './routing'

type Fulfilled = Parameters<RoutableRoute['fulfill']>[0]

function fakeRoute(request: {
  url: string
  method?: string
  headers?: Record<string, string>
  body?: string
}) {
  const result: { fulfilled?: Fulfilled; aborted?: string } = {}
  const route: RoutableRoute = {
    request: () => ({
      url: () => request.url,
      method: () => request.method ?? 'GET',
      allHeaders: () => Promise.resolve(request.headers ?? {}),
      postDataBuffer: () => (request.body === undefined ? null : Buffer.from(request.body)),
    }),
    fulfill: (options) => {
      result.fulfilled = options
      return Promise.resolve()
    },
    abort: (errorCode) => {
      result.aborted = errorCode
      return Promise.resolve()
    },
  }
  return { route, result }
}

function response(status: number, headers: Record<string, string>, body: string): DecoyResponse {
  return {
    status: () => status,
    headers: () => headers,
    body: () => Promise.resolve(Buffer.from(body)),
  }
}

describe('forwardToDecoy', () => {
  test('sends method, exact path/query, headers, and body to Decoy with the Session', async () => {
    const { route } = fakeRoute({
      url: 'https://app.example.com/api/users?id=1&x=%20y',
      method: 'POST',
      headers: {
        ':authority': 'app.example.com',
        host: 'app.example.com',
        'content-length': '13',
        'content-type': 'application/json',
        'x-mock-session': 'spoofed',
      },
      body: '{"name":"Ada"}',
    })
    const calls: { url: string; init: Parameters<DecoyFetch>[1] }[] = []
    const fetch: DecoyFetch = (url, init) => {
      calls.push({ url, init })
      return Promise.resolve(response(200, {}, ''))
    }

    await forwardToDecoy(route, 'http://127.0.0.1:8080', 'session-1', fetch)

    expect(calls).toHaveLength(1)
    expect(calls[0]?.url).toBe('http://127.0.0.1:8080/api/users?id=1&x=%20y')
    expect(calls[0]?.init.method).toBe('POST')
    expect(calls[0]?.init.headers).toEqual({
      'content-type': 'application/json',
      'x-mock-session': 'session-1',
    })
    expect(calls[0]?.init.data?.toString()).toBe('{"name":"Ada"}')
    expect(calls[0]?.init.maxRedirects).toBe(0)
  })

  test('fulfills the browser with Decoy status, headers, and body, misses included', async () => {
    const { route, result } = fakeRoute({ url: 'http://localhost:3000/api/missing' })
    const fetch: DecoyFetch = () =>
      Promise.resolve(
        response(
          501,
          { 'x-mock-miss': 'true' },
          '{"error":"no active route case matched request"}',
        ),
      )

    await forwardToDecoy(route, 'http://127.0.0.1:8080', 's', fetch)

    expect(result.fulfilled?.status).toBe(501)
    expect(result.fulfilled?.headers).toEqual({ 'x-mock-miss': 'true' })
    expect(result.fulfilled?.body?.toString()).toBe(
      '{"error":"no active route case matched request"}',
    )
  })

  test('aborts instead of reaching the network when Decoy cannot be reached', async () => {
    const { route, result } = fakeRoute({ url: 'http://localhost:3000/api/users' })
    const fetch: DecoyFetch = () => Promise.reject(new Error('ECONNREFUSED'))

    await forwardToDecoy(route, 'http://127.0.0.1:1', 's', fetch)

    expect(result.aborted).toBe('connectionrefused')
    expect(result.fulfilled).toBeUndefined()
  })
})

describe('routeToDecoy', () => {
  test('routes every configured pattern to Decoy', async () => {
    const patterns: (string | RegExp)[] = []
    const context = {
      route: (url: string | RegExp) => {
        patterns.push(url)
        return Promise.resolve()
      },
    }
    const fetch: DecoyFetch = () => Promise.resolve(response(200, {}, ''))

    await routeToDecoy(context, fetch, 'http://127.0.0.1:8080', '**/api/**', 's')
    await routeToDecoy(context, fetch, 'http://127.0.0.1:8080', ['/a/', /\/graphql$/], 's')

    expect(patterns).toEqual(['**/api/**', '/a/', /\/graphql$/])
  })
})
