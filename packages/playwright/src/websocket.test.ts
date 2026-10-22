import { describe, expect, test } from '@rstest/core'
import {
  bridgeToDecoy,
  type DecoySocket,
  type OpenDecoySocket,
  type RoutableWebSocket,
  routeWebSocketsToDecoy,
} from './websocket'

function fakePageSocket(url: string) {
  const page = {
    sent: [] as Array<string | Buffer>,
    closed: undefined as { code?: number; reason?: string } | undefined,
    // Set by the bridge through `onMessage` / `onClose`; calling them plays the page's side.
    message: undefined as unknown as (message: string | Buffer) => void,
    close: undefined as unknown as (code: number | undefined, reason: string | undefined) => void,
  }
  const socket: RoutableWebSocket = {
    url: () => url,
    send: (message) => {
      page.sent.push(message)
    },
    close: (options = {}) => {
      page.closed = options
      return Promise.resolve()
    },
    onMessage: (handler) => {
      page.message = handler
    },
    onClose: (handler) => {
      page.close = handler
    },
  }
  return { socket, page }
}

function fakeDecoySocket() {
  const opened: Array<{ url: string; headers: Record<string, string> }> = []
  const decoy = {
    readyState: 0 as number,
    binaryType: 'blob',
    sent: [] as unknown[],
    closed: undefined as { code?: number; reason?: string } | undefined,
    send(data: unknown) {
      this.sent.push(data)
    },
    close(code?: number, reason?: string) {
      this.closed = { code, reason }
    },
    onopen: null as (() => void) | null,
    onmessage: null as ((event: { data: unknown }) => void) | null,
    onclose: null as ((event: { code: number; reason: string }) => void) | null,
    open() {
      this.readyState = 1
      this.onopen?.()
    },
  }
  const open: OpenDecoySocket = (url, init) => {
    opened.push({ url, headers: init.headers })
    return decoy as unknown as DecoySocket
  }
  return { open, opened, decoy }
}

describe('bridgeToDecoy', () => {
  test('opens a socket to Decoy at the page path and query, with the Session', () => {
    const { socket } = fakePageSocket('wss://app.example.com/subscribe?service=friend&x=%20y')
    const { open, opened } = fakeDecoySocket()

    bridgeToDecoy(socket, 'https://127.0.0.1:8443', 'session-1', open)

    expect(opened).toEqual([
      {
        url: 'wss://127.0.0.1:8443/subscribe?service=friend&x=%20y',
        headers: { 'x-mock-session': 'session-1' },
      },
    ])
  })

  test('pipes text and binary both ways, holding page messages until Decoy is open', () => {
    const { socket, page } = fakePageSocket('ws://localhost:3000/ws')
    const { open, opened, decoy } = fakeDecoySocket()
    bridgeToDecoy(socket, 'http://127.0.0.1:8080', 's', open)
    expect(opened[0]?.url).toBe('ws://127.0.0.1:8080/ws')

    page.message('early')
    expect(decoy.sent).toEqual([])
    decoy.open()
    page.message(Buffer.from([1, 2]))
    decoy.onmessage?.({ data: 'hello' })
    decoy.onmessage?.({ data: new Uint8Array([0, 255]).buffer })

    expect(decoy.sent).toEqual(['early', Buffer.from([1, 2])])
    expect(page.sent).toEqual(['hello', Buffer.from([0, 255])])
  })

  test('closes the page socket with the code and reason Decoy closed with', () => {
    const { socket, page } = fakePageSocket('ws://localhost:3000/ws')
    const { open, decoy } = fakeDecoySocket()
    bridgeToDecoy(socket, 'http://127.0.0.1:8080', 's', open)
    decoy.open()

    decoy.onclose?.({ code: 4001, reason: 'unauthorized' })

    expect(page.closed).toEqual({ code: 4001, reason: 'unauthorized' })
  })

  test('closes the page socket, never the real server, when Decoy cannot be reached', () => {
    const { socket, page } = fakePageSocket('ws://localhost:3000/ws')
    const { open, decoy } = fakeDecoySocket()
    bridgeToDecoy(socket, 'http://127.0.0.1:1', 's', open)

    decoy.onclose?.({ code: 1006, reason: '' })

    expect(page.closed).toEqual({})
  })

  test('closes the Decoy socket when the page closes, forwarding a valid code', () => {
    const forwarded = fakePageSocket('ws://localhost:3000/ws')
    const first = fakeDecoySocket()
    bridgeToDecoy(forwarded.socket, 'http://127.0.0.1:8080', 's', first.open)
    forwarded.page.close(1000, 'done')

    const dropped = fakePageSocket('ws://localhost:3000/ws')
    const second = fakeDecoySocket()
    bridgeToDecoy(dropped.socket, 'http://127.0.0.1:8080', 's', second.open)
    dropped.page.close(1005, undefined)

    expect(first.decoy.closed).toEqual({ code: 1000, reason: 'done' })
    expect(second.decoy.closed).toEqual({ code: undefined, reason: undefined })
  })
})

describe('routeWebSocketsToDecoy', () => {
  test('routes every configured pattern', async () => {
    const patterns: Array<string | RegExp> = []
    const context = {
      routeWebSocket: (url: string | RegExp) => {
        patterns.push(url)
        return Promise.resolve()
      },
    }

    await routeWebSocketsToDecoy(context, 'http://127.0.0.1:8080', '**/ws', 's')
    await routeWebSocketsToDecoy(context, 'http://127.0.0.1:8080', ['/a/', /\/live$/], 's')

    expect(patterns).toEqual(['**/ws', '/a/', /\/live$/])
  })
})

describe('the default socket', () => {
  test('sends the Session header on the upgrade to Decoy', async () => {
    const { createServer } = await import('node:http')
    const seen: Array<{ url?: string; session?: string | string[] }> = []
    const server = createServer()
    server.on('upgrade', (req, socket) => {
      seen.push({ url: req.url, session: req.headers['x-mock-session'] })
      socket.destroy()
    })
    await new Promise<void>((done) => server.listen(0, '127.0.0.1', done))
    const address = server.address()
    const decoyUrl = `http://127.0.0.1:${typeof address === 'object' && address ? address.port : 0}`
    try {
      const { socket, page } = fakePageSocket('ws://localhost:3000/subscribe?a=1')
      const closed = new Promise<void>((done) => {
        socket.close = (options = {}) => {
          page.closed = options
          done()
          return Promise.resolve()
        }
      })
      bridgeToDecoy(socket, decoyUrl, 'session-1', undefined)
      await closed

      expect(seen).toEqual([{ url: '/subscribe?a=1', session: 'session-1' }])
      expect(page.closed).toEqual({})
    } finally {
      await new Promise((done) => server.close(done))
    }
  })
})
