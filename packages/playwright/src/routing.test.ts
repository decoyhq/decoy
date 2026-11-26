import { createServer } from 'node:http'
import type { IncomingHttpHeaders } from 'node:http'
import { describe, expect, test } from '@rstest/core'
import { fetchWithoutCookies, forwardToDecoy, routeToDecoy } from './routing'
import type { DecoyFetch, DecoyResponse, RoutableRoute } from './routing'

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
    const calls: Array<{ url: string; init: Parameters<DecoyFetch>[1] }> = []
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
    const patterns: Array<string | RegExp> = []
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

describe('fetchWithoutCookies', () => {
  test('keeps no cookie jar, passes every Set-Cookie and binary bodies, follows no redirect', async () => {
    const seen: IncomingHttpHeaders[] = []
    const server = createServer((req, res) => {
      seen.push(req.headers)
      res.writeHead(302, { location: '/elsewhere', 'set-cookie': ['a=1; Path=/', 'b=2; Path=/'] })
      res.end(Buffer.from([0, 255, 1]))
    })
    await new Promise<void>((done) => {
      server.listen(0, '127.0.0.1', done)
    })
    const address = server.address()
    const decoyUrl = `http://127.0.0.1:${typeof address === 'object' && address ? address.port : 0}`
    try {
      const first = fakeRoute({ url: 'http://localhost:3000/login' })
      await forwardToDecoy(first.route, decoyUrl, 's', fetchWithoutCookies)
      const second = fakeRoute({ url: 'http://localhost:3000/me' })
      await forwardToDecoy(second.route, decoyUrl, 's', fetchWithoutCookies)

      expect(first.result.fulfilled?.status).toBe(302)
      expect(first.result.fulfilled?.headers?.['set-cookie']).toBe('a=1; Path=/\nb=2; Path=/')
      expect(first.result.fulfilled?.body).toEqual(Buffer.from([0, 255, 1]))
      expect(seen.map((headers) => headers.cookie)).toEqual([undefined, undefined])
    } finally {
      await new Promise((done) => {
        server.close(done)
      })
    }
  })
})
