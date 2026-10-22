import type { ServerOptions } from '@decoyhq/runner-core'

/** URL patterns: one Playwright glob or RegExp, or a list of them. */
export type UrlPatterns = string | RegExp | ReadonlyArray<string | RegExp>

export function patternList(urls: UrlPatterns): ReadonlyArray<string | RegExp> {
  return typeof urls === 'string' || urls instanceof RegExp ? [urls] : urls
}

/** Decoy options for a Playwright project, set under `use.decoyOptions`. */
export interface DecoyOptions extends ServerOptions {
  /**
   * Browser request URL patterns (Playwright `context.route` globs or RegExps) to send to Decoy.
   * Matching requests always go to Decoy; everything else continues normally.
   */
  urls?: UrlPatterns
  /**
   * Page WebSocket URL patterns (Playwright `context.routeWebSocket` globs or RegExps) to bridge to
   * Decoy. Matching sockets connect to Decoy only, never to the real server.
   */
  webSocketUrls?: UrlPatterns
  /** Collection each test's Session starts on, applied before any routed request. */
  collection?: string
}
