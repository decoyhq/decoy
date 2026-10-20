import { SESSION_HEADER } from '@decoyhq/runner-core'
import type { DecoyOptions } from './options'

/** The slice of a Puppeteer `HTTPRequest` routing needs. */
export interface InterceptedRequest {
  url(): string
  headers(): Record<string, string>
  continue(overrides?: { url?: string; headers?: Record<string, string> }): Promise<void>
  abort(errorCode?: 'connectionrefused' | 'failed'): Promise<void>
}

/** The slice of a Puppeteer `Page` routing needs. */
export interface InterceptablePage {
  setRequestInterception(enabled: boolean): Promise<void>
  on(event: 'request', handler: (request: InterceptedRequest) => void): unknown
}

/** The slice of a Puppeteer `Browser` (from `browser.getPuppeteer()`) routing needs. */
export interface InterceptableBrowser {
  pages(): Promise<InterceptablePage[]>
  on(
    event: 'targetcreated',
    handler: (target: { page(): Promise<InterceptablePage | null> }) => void,
  ): unknown
}

/** Which Session matching requests belong to right now; `undefined` between tests. */
export interface SessionHolder {
  sessionId: string | undefined
}

/**
 * Send every request matching `urls`, from every page of `browser` (current and future), to Decoy
 * at `decoyUrl`, stamped with the holder's current Session. The rewrite happens inside Chrome
 * (CDP `Fetch.continueRequest` with a new url), so method, headers, and body are kept and the page
 * still sees its own origin. Requests outside a test are aborted, never sent to the real network.
 */
export async function routeToDecoy(
  browser: InterceptableBrowser,
  decoyUrl: string,
  urls: NonNullable<DecoyOptions['urls']>,
  holder: SessionHolder,
): Promise<void> {
  const matchers = toMatchers(urls)
  const install = async (page: InterceptablePage) => {
    await page.setRequestInterception(true)
    page.on('request', (request) => {
      void forwardToDecoy(request, decoyUrl, matchers, holder)
    })
  }
  browser.on('targetcreated', (target) => {
    void target
      .page()
      .then((page) => (page ? install(page) : undefined))
      .catch(() => undefined)
  })
  await Promise.all((await browser.pages()).map(install))
}

export async function forwardToDecoy(
  request: InterceptedRequest,
  decoyUrl: string,
  matchers: readonly RegExp[],
  holder: SessionHolder,
): Promise<void> {
  const original = request.url()
  try {
    if (!matchers.some((matcher) => matcher.test(original))) {
      await request.continue()
      return
    }
    if (holder.sessionId === undefined) {
      await request.abort('failed')
      return
    }
    const { pathname, search } = new URL(original)
    await request.continue({
      url: `${decoyUrl}${pathname}${search}`,
      headers: { ...request.headers(), [SESSION_HEADER]: holder.sessionId },
    })
  } catch {
    // The page navigated or closed while the request was paused; nothing is left to answer.
  }
}

/** Playwright-style globs (`*` within a path segment, `**` across them) or RegExps. */
export function toMatchers(urls: NonNullable<DecoyOptions['urls']>): RegExp[] {
  const patterns = typeof urls === 'string' || urls instanceof RegExp ? [urls] : urls
  return patterns.map((pattern) => (typeof pattern === 'string' ? globToRegExp(pattern) : pattern))
}

function globToRegExp(glob: string): RegExp {
  const source = glob
    .split('**')
    .map((part) =>
      part
        .split('*')
        .map((literal) => literal.replace(/[.+?^${}()|[\]\\/]/g, '\\$&'))
        .join('[^/]*'),
    )
    .join('.*')
  return new RegExp(`^${source}$`)
}
