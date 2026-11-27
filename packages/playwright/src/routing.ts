import type { Request, Route } from '@playwright/test'
import { SESSION_HEADER } from '@decoyhq/runner-core'
import { patternList } from './options'
import type { UrlPatterns } from './options'

/** The response shape forwarding needs; Playwright's `APIResponse` satisfies it. */
export interface DecoyResponse {
  status(): number
  headers(): Record<string, string>
  body(): Promise<Buffer>
}

export type DecoyFetch = (
  url: string,
  init: { method: string; headers: Record<string, string>; data?: Buffer },
) => Promise<DecoyResponse>

export interface RoutableRoute extends Pick<Route, 'fulfill' | 'abort'> {
  request(): Pick<Request, 'url' | 'method' | 'allHeaders' | 'postDataBuffer'>
}

/** The routing surface needed from a `BrowserContext`, which satisfies it. */
export interface RoutableContext {
  route(url: string | RegExp, handler: (route: Route) => Promise<void>): Promise<unknown>
}

/**
 * Headers the fetch to Decoy must compute itself rather than copy from the browser request; Node's
 * `fetch` rejects the hop-by-hop ones.
 */
const HOP_HEADERS = new Set([
  'host',
  'content-length',
  'connection',
  'keep-alive',
  'transfer-encoding',
  'upgrade',
  'expect',
])

/**
 * Forwards with Node's `fetch`, which keeps no cookie jar: a Set-Cookie from Decoy reaches only the
 * browser (for the original origin), and a request without cookies reaches Decoy without cookies.
 * `context.request` would share the browser context's jar, keyed by Decoy's host.
 */
export const fetchWithoutCookies: DecoyFetch = async (url, { method, headers, data }) => {
  const response = await fetch(url, { method, headers, body: data, redirect: 'manual' })
  const body = Buffer.from(await response.arrayBuffer())
  const responseHeaders = Object.fromEntries(response.headers)
  const cookies = response.headers.getSetCookie()
  if (cookies.length > 0) {
    // Playwright's `fulfill` splits `set-cookie` on newlines, as `APIResponse.headers()` joins it.
    responseHeaders['set-cookie'] = cookies.join('\n')
  }
  return {
    status: () => response.status,
    headers: () => responseHeaders,
    body: () => Promise.resolve(body),
  }
}

/** Install browser-context routing that sends every `urls` pattern to Decoy at `decoyUrl`. */
export async function routeToDecoy(
  context: RoutableContext,
  fetch: DecoyFetch,
  decoyUrl: string,
  urls: UrlPatterns,
  sessionId: string,
): Promise<void> {
  for (const pattern of patternList(urls)) {
    await context.route(pattern, (route) => forwardToDecoy(route, decoyUrl, sessionId, fetch))
  }
}

/**
 * Send one routed browser request to Decoy, keeping method, path/query, headers, and body, and
 * stamping the Session. Decoy's answer, including a fail-closed miss, is fulfilled as-is. If Decoy
 * cannot be reached the request is aborted, never continued to the real network.
 */
export async function forwardToDecoy(
  route: RoutableRoute,
  decoyUrl: string,
  sessionId: string,
  fetch: DecoyFetch,
): Promise<void> {
  const request = route.request()
  const original = new URL(request.url())
  const headers: Record<string, string> = {}
  for (const [name, value] of Object.entries(await request.allHeaders())) {
    if (!name.startsWith(':') && !HOP_HEADERS.has(name)) {
      headers[name] = value
    }
  }
  headers[SESSION_HEADER] = sessionId

  let response: DecoyResponse
  try {
    response = await fetch(`${decoyUrl}${original.pathname}${original.search}`, {
      method: request.method(),
      headers,
      data: request.postDataBuffer() ?? undefined,
    })
  } catch {
    await route.abort('connectionrefused')
    return
  }
  await route.fulfill({
    status: response.status(),
    headers: response.headers(),
    body: await response.body(),
  })
}
