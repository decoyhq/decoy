import type { WebSocketRoute } from '@playwright/test'
import type { DecoyOptions } from './options'

import { SESSION_HEADER } from '@decoyhq/runner-core'

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

/** Install browser-context routing that bridges every `webSocketUrls` socket to Decoy. */
export async function routeWebSocketsToDecoy(
  context: WebSocketRoutableContext,
  decoyUrl: string,
  urls: NonNullable<DecoyOptions['webSocketUrls']>,
  sessionId: string,
): Promise<void> {
  const patterns = typeof urls === 'string' || urls instanceof RegExp ? [urls] : urls
  for (const pattern of patterns) {
    await context.routeWebSocket(pattern, (socket) => {
      bridgeToDecoy(socket, decoyUrl, sessionId)
    })
  }
}

/**
 * Bridge one page socket to Decoy: a socket of our own to the same path and query on Decoy, carrying
 * the Session, with frames piped both ways and each side closed when the other closes. The page
 * socket never reaches the real server; if Decoy can't be reached it is closed.
 */
export function bridgeToDecoy(
  page: RoutableWebSocket,
  decoyUrl: string,
  sessionId: string,
  open: OpenDecoySocket = openWebSocket,
): void {
  const original = new URL(page.url())
  const url = new URL(`${original.pathname}${original.search}`, decoyUrl)
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:'
  const decoy = open(url.href, { headers: { [SESSION_HEADER]: sessionId } })
  decoy.binaryType = 'arraybuffer'

  const pending: Array<string | Buffer> = []
  decoy.onopen = () => {
    for (const message of pending.splice(0)) {
      decoy.send(message)
    }
  }
  page.onMessage((message) => {
    if (decoy.readyState === WebSocket.CONNECTING) {
      pending.push(message)
    } else if (decoy.readyState === WebSocket.OPEN) {
      decoy.send(message)
    }
  })
  decoy.onmessage = ({ data }: MessageEvent) => {
    page.send(typeof data === 'string' ? data : Buffer.from(data as ArrayBuffer))
  }

  decoy.onclose = ({ code, reason }: CloseEvent) => {
    void page.close(canSend(code) ? { code, reason } : {})
  }
  page.onClose((code, reason) => {
    if (code !== undefined && canSend(code)) {
      decoy.close(code, reason)
    } else {
      decoy.close()
    }
  })
}

/** Close codes an endpoint may send; the rest (1005, 1006, ...) only describe a closed socket. */
function canSend(code: number): boolean {
  return code === 1000 || (code >= 3000 && code <= 4999)
}
