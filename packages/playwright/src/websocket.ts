import type { WebSocketRoute } from '@playwright/test'
import { SESSION_HEADER } from '@decoyhq/runner-core'
import { patternList } from './options'
import type { UrlPatterns } from './options'

/** The page side of a routed socket; Playwright's `WebSocketRoute` satisfies it. */
export type RoutableWebSocket = Pick<
  WebSocketRoute,
  'url' | 'send' | 'close' | 'onMessage' | 'onClose'
>

/** The WebSocket-routing surface needed from a `BrowserContext`, which satisfies it. */
export interface WebSocketRoutableContext {
  routeWebSocket(
    url: string | RegExp,
    handler: (socket: WebSocketRoute) => unknown,
  ): Promise<unknown>
}

/** The Decoy side of a bridge; Node's global `WebSocket` satisfies it. */
export type DecoySocket = Pick<
  WebSocket,
  'readyState' | 'binaryType' | 'send' | 'close' | 'onopen' | 'onmessage' | 'onclose'
>

export type OpenDecoySocket = (
  url: string,
  init: { headers: Record<string, string> },
) => DecoySocket

// Node's WebSocket (undici) takes `headers` in its init, unlike the browser's.
const openWebSocket: OpenDecoySocket = (url, init) => new WebSocket(url, init)

/**
 * Install browser-context routing that bridges every `webSocketUrls` socket to Decoy. Returns
 * `closeAll`, which closes the Decoy sockets still open, so none outlives the test even if a page
 * socket's close never reaches the bridge.
 */
export async function routeWebSocketsToDecoy(
  context: WebSocketRoutableContext,
  decoyUrl: string,
  urls: UrlPatterns,
  sessionId: string,
  open: OpenDecoySocket = openWebSocket,
): Promise<() => void> {
  const live = new Set<DecoySocket>()
  for (const pattern of patternList(urls)) {
    await context.routeWebSocket(pattern, (socket) => {
      bridgeToDecoy(socket, decoyUrl, sessionId, open, live)
    })
  }
  return () => {
    for (const decoySocket of live) {
      decoySocket.close()
    }
    live.clear()
  }
}

/**
 * Bridge one page socket to Decoy: a socket of our own to the same path and query on Decoy, carrying
 * the Session, with frames piped both ways and each side closed when the other closes. The page
 * socket never reaches the real server; if Decoy can't be reached it is closed. The Decoy socket
 * is in `live` until it closes.
 */
export function bridgeToDecoy(
  pageSocket: RoutableWebSocket,
  decoyUrl: string,
  sessionId: string,
  open: OpenDecoySocket = openWebSocket,
  live = new Set<DecoySocket>(),
): void {
  const original = new URL(pageSocket.url())
  // A string join, like HTTP routing: it keeps a base path in `decoyUrl`.
  const url = new URL(`${decoyUrl}${original.pathname}${original.search}`)
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:'
  const decoySocket = open(url.href, { headers: { [SESSION_HEADER]: sessionId } })
  decoySocket.binaryType = 'arraybuffer'
  live.add(decoySocket)

  const pending: Array<string | Buffer> = []
  decoySocket.onopen = () => {
    for (const message of pending.splice(0)) {
      decoySocket.send(message)
    }
  }
  pageSocket.onMessage((message) => {
    if (decoySocket.readyState === WebSocket.CONNECTING) {
      pending.push(message)
    } else if (decoySocket.readyState === WebSocket.OPEN) {
      decoySocket.send(message)
    }
  })
  decoySocket.onmessage = ({ data }: MessageEvent) => {
    pageSocket.send(typeof data === 'string' ? data : Buffer.from(data as ArrayBuffer))
  }

  decoySocket.onclose = ({ code, reason }: CloseEvent) => {
    live.delete(decoySocket)
    void pageSocket.close(canSend(code) ? { code, reason } : {})
  }
  pageSocket.onClose((code, reason) => {
    if (code !== undefined && canSend(code)) {
      decoySocket.close(code, reason)
    } else {
      decoySocket.close()
    }
  })
}

/** Close codes an endpoint may send; the rest (1005, 1006, ...) only describe a closed socket. */
function canSend(code: number): boolean {
  return code === 1000 || (code >= 3000 && code <= 4999)
}
