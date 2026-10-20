import { SESSION_HEADER } from '@decoyhq/runner-core'
import { describe, expect, test } from '@rstest/core'
import {
  type CdpSession,
  decide,
  forwardToDecoy,
  type RequestPaused,
  routeToDecoy,
  toMatchers,
} from './routing'

const decoyUrl = 'http://127.0.0.1:8080'

interface FakeSession {
  session: CdpSession
  sent: Array<[string, unknown]>
  emit: (event: string, payload: unknown) => void
  children: Map<string, FakeSession>
}

/** A CDP session that records what it was sent and lets tests emit events to it. */
function fakeSession(fail: string[] = []): FakeSession {
  const sent: Array<[string, unknown]> = []
  const handlers = new Map<string, (event: never) => void>()
  const children = new Map<string, FakeSession>()
  const session: CdpSession = {
    send: (method, params) => {
      sent.push([method, params])
      return fail.includes(method) ? Promise.reject(new Error(method)) : Promise.resolve({})
    },
    on: (event, handler) => {
      handlers.set(event, handler)
    },
    connection: () => ({ session: (id) => children.get(id)?.session ?? null }),
  }
  const emit = (event: string, payload: unknown) => {
    ;(handlers.get(event) as ((event: unknown) => void) | undefined)?.(payload)
  }
  return { session, sent, emit, children }
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 0))

describe('toMatchers', () => {
  test('globs: `*` stays in a segment, `**` crosses them', () => {
    const [api] = toMatchers('**/api/**')
    expect(api?.test('http://app.test/api/users/1?x=1')).toBe(true)
    expect(api?.test('http://app.test/apix/users')).toBe(false)
    const [one] = toMatchers('http://app.test/*/users')
    expect(one?.test('http://app.test/v1/users')).toBe(true)
    expect(one?.test('http://app.test/v1/v2/users')).toBe(false)
  })

  test('`{a,b}` matches either alternative', () => {
    const [either] = toMatchers('**/{api,graphql}/**')
    expect(either?.test('http://app.test/api/users')).toBe(true)
    expect(either?.test('http://app.test/graphql/q')).toBe(true)
    expect(either?.test('http://app.test/rest/q')).toBe(false)
    expect(either?.test('http://app.test/{api,graphql}/q')).toBe(false)
  })

  test('throws on a glob that would never match a full URL', () => {
    expect(() => toMatchers('/api/**')).toThrow(/must start with `\*` or a scheme/)
    expect(() => toMatchers(['**/api/**', 'app.test/**'])).toThrow(/app\.test/)
    expect(() => toMatchers('**/{api,{a,b}}/**')).toThrow(/nested/)
    expect(() => toMatchers('**/{api/**')).toThrow(/unclosed/)
  })

  test('RegExps pass through', () => {
    expect(toMatchers([/users/])[0]?.test('http://app.test/users')).toBe(true)
  })

  test('a global or sticky RegExp matches every request, not every other one', () => {
    for (const pattern of [/users/g, /users/y, /USERS/gi]) {
      const [matcher] = toMatchers(pattern)
      const url = pattern.sticky ? 'users' : 'http://app.test/users'
      expect([matcher?.test(url), matcher?.test(url)]).toEqual([true, true])
    }
    expect(toMatchers(/users/gi)[0]?.flags).toBe('i')
  })
})

describe('decide', () => {
  const matchers = toMatchers('**/api/**')

  test('sends a matching request to Decoy under the current Session, keeping path and query', () => {
    expect(
      decide('http://app.test/api/users/1?x=1', decoyUrl, matchers, { sessionId: 's-1' }),
    ).toEqual({ kind: 'decoy', url: 'http://127.0.0.1:8080/api/users/1?x=1', sessionId: 's-1' })
  })

  test('continues other requests', () => {
    expect(decide('http://app.test/index.html', decoyUrl, matchers, { sessionId: 's-1' })).toEqual({
      kind: 'continue',
    })
  })

  test('aborts a matching request outside a test instead of sending it anywhere', () => {
    expect(
      decide('http://app.test/api/users/1', decoyUrl, matchers, { sessionId: undefined }),
    ).toEqual({ kind: 'abort' })
  })
})

describe('forwardToDecoy', () => {
  const paused: RequestPaused = {
    requestId: 'r-1',
    request: {
      url: 'http://app.test/api/users/1',
      headers: { accept: 'a/b', 'X-Mock-Session': 'stale' },
    },
  }

  test('rewrites the url and replaces any Session header with the current one', async () => {
    const { session, sent } = fakeSession()
    await forwardToDecoy(session, paused, {
      kind: 'decoy',
      url: 'http://127.0.0.1:8080/api/users/1',
      sessionId: 's-1',
    })
    expect(sent).toEqual([
      [
        'Fetch.continueRequest',
        {
          requestId: 'r-1',
          url: 'http://127.0.0.1:8080/api/users/1',
          headers: [
            { name: 'accept', value: 'a/b' },
            { name: SESSION_HEADER, value: 's-1' },
          ],
          interceptResponse: true,
        },
      ],
    ])
  })

  test('continues untouched or fails the request', async () => {
    const { session, sent } = fakeSession()
    await forwardToDecoy(session, paused, { kind: 'continue' })
    await forwardToDecoy(session, paused, { kind: 'abort' })
    expect(sent).toEqual([
      ['Fetch.continueRequest', { requestId: 'r-1' }],
      ['Fetch.failRequest', { requestId: 'r-1', errorReason: 'Failed' }],
    ])
  })
})

describe('routeToDecoy', () => {
  const attach = (type: string, waitingForDebugger = true) => ({
    sessionId: 'child',
    targetInfo: { targetId: 't-1', type },
    waitingForDebugger,
  })

  test('auto-attaches with new targets paused', async () => {
    const root = fakeSession()
    await routeToDecoy(root.session, decoyUrl, '**/api/**', { sessionId: 's-1' })
    expect(root.sent).toEqual([
      ['Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true }],
    ])
  })

  test('a new page runs only after interception is on, and its requests are routed', async () => {
    const root = fakeSession()
    const page = fakeSession()
    root.children.set('child', page)
    await routeToDecoy(root.session, decoyUrl, '**/api/**', { sessionId: 's-1' })

    root.emit('Target.attachedToTarget', attach('page'))
    await settle()
    expect(page.sent.map(([method]) => method)).toEqual([
      'Fetch.enable',
      'Target.setAutoAttach',
      'Runtime.runIfWaitingForDebugger',
    ])

    page.emit('Fetch.requestPaused', {
      requestId: 'r-1',
      request: { url: 'http://app.test/api/users/1', headers: {} },
    })
    await settle()
    expect(page.sent.at(-1)).toEqual([
      'Fetch.continueRequest',
      {
        requestId: 'r-1',
        url: 'http://127.0.0.1:8080/api/users/1',
        headers: [{ name: SESSION_HEADER, value: 's-1' }],
        interceptResponse: true,
      },
    ])
  })

  test("a routed redirect's relative Location resolves against the page's URL, not Decoy's", async () => {
    const root = fakeSession()
    const page = fakeSession()
    root.children.set('child', page)
    await routeToDecoy(root.session, decoyUrl, '**/api/**', { sessionId: 's-1' })
    root.emit('Target.attachedToTarget', attach('page'))
    await settle()

    page.emit('Fetch.requestPaused', {
      requestId: 'r-1',
      request: { url: 'http://app.test/api/old?x=1', headers: {} },
    })
    page.emit('Fetch.requestPaused', {
      requestId: 'r-1',
      request: { url: 'http://127.0.0.1:8080/api/old?x=1', headers: {} },
      responseStatusCode: 302,
      responseHeaders: [
        { name: 'Location', value: 'new' },
        { name: 'x-other', value: 'kept' },
      ],
    })
    await settle()
    expect(page.sent.at(-1)).toEqual([
      'Fetch.continueRequest',
      { requestId: 'r-1', url: 'http://app.test/api/new' },
    ])
  })

  test('a routed response that is not a redirect, or that failed, is continued untouched', async () => {
    const root = fakeSession()
    const page = fakeSession()
    root.children.set('child', page)
    await routeToDecoy(root.session, decoyUrl, '**/api/**', { sessionId: 's-1' })
    root.emit('Target.attachedToTarget', attach('page'))
    await settle()

    for (const [requestId, response] of [
      ['r-1', { responseStatusCode: 201, responseHeaders: [{ name: 'location', value: 'x' }] }],
      ['r-2', { responseErrorReason: 'ConnectionRefused' }],
    ] as const) {
      page.emit('Fetch.requestPaused', {
        requestId,
        request: { url: 'http://app.test/api/users/1', headers: {} },
      })
      page.emit('Fetch.requestPaused', {
        requestId,
        request: { url: 'http://127.0.0.1:8080/api/users/1', headers: {} },
        ...response,
      })
    }
    await settle()
    // Each response follows its own routed request (`at(-2)` is r-2's).
    expect([page.sent.at(-3), page.sent.at(-1)]).toEqual([
      ['Fetch.continueRequest', { requestId: 'r-1' }],
      ['Fetch.continueRequest', { requestId: 'r-2' }],
    ])
  })

  test('a page whose interception fails is closed, never resumed', async () => {
    const root = fakeSession()
    const page = fakeSession(['Fetch.enable'])
    root.children.set('child', page)
    await routeToDecoy(root.session, decoyUrl, '**/api/**', { sessionId: 's-1' })

    root.emit('Target.attachedToTarget', attach('page'))
    await settle()
    expect(page.sent.map(([method]) => method)).toEqual(['Fetch.enable'])
    expect(root.sent.at(-1)).toEqual(['Target.closeTarget', { targetId: 't-1' }])
  })

  test.each(['shared_worker', 'service_worker'])(
    'a %s runs only after interception is on',
    async (type) => {
      const root = fakeSession()
      const worker = fakeSession()
      root.children.set('child', worker)
      await routeToDecoy(root.session, decoyUrl, '**/api/**', { sessionId: 's-1' })

      root.emit('Target.attachedToTarget', attach(type))
      await settle()
      expect(worker.sent.map(([method]) => method)).toEqual([
        'Fetch.enable',
        'Target.setAutoAttach',
        'Runtime.runIfWaitingForDebugger',
      ])
    },
  )

  test('other targets (dedicated workers, routed by their page) are only resumed', async () => {
    const root = fakeSession()
    const other = fakeSession()
    root.children.set('child', other)
    await routeToDecoy(root.session, decoyUrl, '**/api/**', { sessionId: 's-1' })

    root.emit('Target.attachedToTarget', attach('worker'))
    await settle()
    expect(other.sent).toEqual([['Runtime.runIfWaitingForDebugger', undefined]])
  })
})
