import { SESSION_HEADER } from '@decoyhq/runner-core'
import type { DecoyOptions } from './options'

/** The slice of a Puppeteer `CDPSession` routing needs. */
export interface CdpSession {
  send(method: string, params?: object): Promise<unknown>
  on(event: string, handler: (event: never) => void): unknown
  connection(): { session(sessionId: string): CdpSession | null } | undefined
}

interface AttachedToTarget {
  sessionId: string
  targetInfo: { targetId: string; type: string }
  waitingForDebugger: boolean
}

/** The slice of a CDP `Fetch.requestPaused` event routing needs. */
export interface RequestPaused {
  requestId: string
  request: { url: string; headers: Record<string, string> }
  // Present only at the response stage, which routed requests also pause at.
  responseStatusCode?: number
  responseHeaders?: Array<{ name: string; value: string }>
}

/** Which Session matching requests belong to right now; `undefined` between tests. */
export interface SessionHolder {
  sessionId: string | undefined
}

/** What to do with one paused browser request. */
export type Decision =
  { kind: 'continue' } | { kind: 'abort' } | { kind: 'decoy'; url: string; sessionId: string }

// Targets whose requests no page's interception sees. A dedicated `worker` has no Fetch domain;
// its requests pause on the page that started it, so it is only resumed, like any other target.
const ROUTED_TARGETS = new Set(['page', 'iframe', 'shared_worker', 'service_worker'])
const REDIRECTS = new Set([301, 302, 303, 307, 308])

/**
 * Send every request matching `urls`, from every page, out-of-process frame and worker of the browser
 * behind `root` (a browser-level CDP session), to Decoy at `decoyUrl`, stamped with the holder's
 * current Session. New tabs and popups start paused (auto-attach with `waitForDebuggerOnStart`)
 * and only run once interception is on, so even their first request is routed. The rewrite
 * happens inside Chrome (`Fetch.continueRequest` with a new url): method, headers, cookies, and
 * body are kept and the page still sees its own origin. Chrome would resolve a relative redirect
 * from Decoy against Decoy's URL, so routed responses pause too and `Location` is resolved against
 * the page's URL.
 */
export async function routeToDecoy(
  root: CdpSession,
  decoyUrl: string,
  urls: NonNullable<DecoyOptions['urls']>,
  holder: SessionHolder,
): Promise<void> {
  const matchers = toMatchers(urls)
  const autoAttach = (session: CdpSession) => {
    session.on('Target.attachedToTarget', (event: AttachedToTarget) => {
      void intercept(session, event)
    })
    return session.send('Target.setAutoAttach', {
      autoAttach: true,
      waitForDebuggerOnStart: true,
      flatten: true,
    })
  }
  const intercept = async (parent: CdpSession, event: AttachedToTarget) => {
    const target = parent.connection()?.session(event.sessionId)
    if (!target) {
      return
    }
    if (ROUTED_TARGETS.has(event.targetInfo.type)) {
      const originals = new Map<string, string>() // requestId of a routed request -> its page URL
      try {
        target.on('Fetch.requestPaused', (paused: RequestPaused) => {
          const original = originals.get(paused.requestId)
          if (original !== undefined) {
            originals.delete(paused.requestId)
            void resumeRouted(target, paused, original)
            return
          }
          const decision = decide(paused.request.url, decoyUrl, matchers, holder)
          if (decision.kind === 'decoy') {
            originals.set(paused.requestId, paused.request.url)
          }
          void forwardToDecoy(target, paused, decision)
        })
        await target.send('Fetch.enable', {
          patterns: [{ urlPattern: '*', requestStage: 'Request' }],
        })
        await autoAttach(target) // out-of-process iframes, a page's workers
      } catch {
        // Unroutable, so it must not run: close it rather than let it reach the real network.
        await root
          .send('Target.closeTarget', { targetId: event.targetInfo.targetId })
          .catch(() => undefined)
        return
      }
    }
    if (event.waitingForDebugger) {
      await target.send('Runtime.runIfWaitingForDebugger').catch(() => undefined)
    }
  }
  await autoAttach(root)
}

/**
 * Matching requests go to Decoy under the holder's current Session, keeping path and query.
 * Matching requests outside a test are aborted, never sent to the real network.
 */
export function decide(
  original: string,
  decoyUrl: string,
  matchers: readonly RegExp[],
  holder: SessionHolder,
): Decision {
  if (!matchers.some((matcher) => matcher.test(original))) {
    return { kind: 'continue' }
  }
  if (holder.sessionId === undefined) {
    return { kind: 'abort' }
  }
  const { pathname, search } = new URL(original)
  return { kind: 'decoy', url: `${decoyUrl}${pathname}${search}`, sessionId: holder.sessionId }
}

export async function forwardToDecoy(
  session: Pick<CdpSession, 'send'>,
  paused: RequestPaused,
  decision: Decision,
): Promise<void> {
  const { requestId } = paused
  try {
    if (decision.kind === 'continue') {
      await session.send('Fetch.continueRequest', { requestId })
    } else if (decision.kind === 'abort') {
      await session.send('Fetch.failRequest', { requestId, errorReason: 'Failed' })
    } else {
      const headers = Object.entries(paused.request.headers)
        .filter(([name]) => name.toLowerCase() !== SESSION_HEADER)
        .map(([name, value]) => ({ name, value }))
      headers.push({ name: SESSION_HEADER, value: decision.sessionId })
      await session.send('Fetch.continueRequest', {
        requestId,
        url: decision.url,
        headers,
        interceptResponse: true,
      })
    }
  } catch {
    // The page navigated or closed while the request was paused; nothing is left to answer.
  }
}

/**
 * Resumes a routed response. A redirect is sent on to its `Location` resolved against the page's URL
 * `original`: Chrome would resolve it against Decoy's, and ignores a rewritten `Location` header.
 */
async function resumeRouted(
  session: Pick<CdpSession, 'send'>,
  paused: RequestPaused,
  original: string,
): Promise<void> {
  const { requestId, responseStatusCode = 0, responseHeaders = [] } = paused
  const location = responseHeaders.find(({ name }) => name.toLowerCase() === 'location')?.value
  const url = REDIRECTS.has(responseStatusCode) && location && URL.parse(location, original)?.href
  try {
    await session.send('Fetch.continueRequest', url ? { requestId, url } : { requestId })
  } catch {
    // The page navigated or closed while the response was paused; nothing is left to answer.
  }
}

/**
 * Playwright-style globs (`*` within a path segment, `**` across them, `{a,b}` for either) or
 * RegExps. Throws on a glob that starts with neither `*` nor a scheme: it could never match.
 */
export function toMatchers(urls: NonNullable<DecoyOptions['urls']>): RegExp[] {
  const patterns = typeof urls === 'string' || urls instanceof RegExp ? [urls] : urls
  // Without `g`/`y`: `test()` on those resumes at `lastIndex`, so every other request would miss.
  return patterns.map((pattern) =>
    typeof pattern === 'string'
      ? globToRegExp(pattern)
      : new RegExp(pattern.source, pattern.flags.replace(/[gy]/g, '')),
  )
}

function globToRegExp(glob: string): RegExp {
  const fail = (reason: string): never => {
    throw new Error(`@decoyhq/testplane: \`urls\` glob '${glob}' ${reason}`)
  }
  // A non-match goes to the real network, so a glob that can never match must not load quietly.
  if (!glob.startsWith('*') && !/^[a-z][a-z\d+.-]*:\/\//i.test(glob)) {
    fail("must start with `*` or a scheme: it is matched against the full URL (try '**/api/**')")
  }
  let source = ''
  let inGroup = false
  for (let i = 0; i < glob.length; i++) {
    const char = glob.charAt(i)
    if (char === '*') {
      const double = glob.charAt(i + 1) === '*'
      source += double ? '.*' : '[^/]*'
      i += double ? 1 : 0
    } else if (char === '{') {
      if (inGroup) fail('has a nested `{`, which is not supported')
      inGroup = true
      source += '(?:'
    } else if (char === '}' && inGroup) {
      inGroup = false
      source += ')'
    } else if (char === ',' && inGroup) {
      source += '|'
    } else {
      source += char.replace(/[.+?^${}()|[\]\\/]/g, '\\$&')
    }
  }
  if (inGroup) fail('has an unclosed `{`')
  return new RegExp(`^${source}$`)
}
