import type { Request, Route } from '@playwright/test'
import { type DecoyOptions, isHealthy, resolveOptions } from './options'

export const SESSION_HEADER = 'x-mock-session'

/** The response shape forwarding needs; Playwright's `APIResponse` satisfies it. */
export interface DecoyResponse {
  status(): number
  headers(): Record<string, string>
  body(): Promise<Buffer>
}

export type DecoyFetch = (
  url: string,
  init: { method: string; headers: Record<string, string>; data?: Buffer; maxRedirects: number },
) => Promise<DecoyResponse>

export interface RoutableRoute extends Pick<Route, 'fulfill' | 'abort'> {
  request(): Pick<Request, 'url' | 'method' | 'allHeaders' | 'postDataBuffer'>
}

/** The routing surface needed from a `BrowserContext`, which satisfies it. */
export interface RoutableContext {
  route(url: string | RegExp, handler: (route: Route) => Promise<void>): Promise<unknown>
}

/** Headers the fetch to Decoy must compute itself rather than copy from the browser request. */
const HOP_HEADERS = new Set(['host', 'content-length', 'connection'])

/**
 * Install browser-context routing for the configured URL patterns. Does nothing when Decoy is not
 * configured or has no `urls`; fails clearly when Decoy is configured but unreachable.
 */
export async function routeToDecoy(
  context: RoutableContext,
  fetch: DecoyFetch,
  decoyOptions: DecoyOptions | undefined,
  sessionId: string,
): Promise<void> {
  const patterns = toPatterns(decoyOptions?.urls)
  if (!decoyOptions || patterns.length === 0) {
    return
  }
  const { url } = resolveOptions(decoyOptions)
  if (!(await isHealthy(url))) {
    throw new Error(
      `Decoy is not reachable at ${url}. Start it with the \`setup\` global setup or run \`decoy serve\`.`,
    )
  }
  for (const pattern of patterns) {
    await context.route(pattern, (route) => forwardToDecoy(route, url, sessionId, fetch))
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
      maxRedirects: 0,
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

function toPatterns(urls: DecoyOptions['urls']): ReadonlyArray<string | RegExp> {
  if (urls === undefined) {
    return []
  }
  return typeof urls === 'string' || urls instanceof RegExp ? [urls] : urls
}
