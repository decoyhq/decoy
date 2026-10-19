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
}

/** Which Session matching requests belong to right now; `undefined` between tests. */
export interface SessionHolder {
  sessionId: string | undefined
}

/** What to do with one paused browser request. */
export type Decision =
  { kind: 'continue' } | { kind: 'abort' } | { kind: 'decoy'; url: string; sessionId: string }

/**
 * Send every request matching `urls`, from every page and out-of-process frame of the browser
 * behind `root` (a browser-level CDP session), to Decoy at `decoyUrl`, stamped with the holder's
 * current Session. New tabs and popups start paused (auto-attach with `waitForDebuggerOnStart`)
 * and only run once interception is on, so even their first request is routed. The rewrite
 * happens inside Chrome (`Fetch.continueRequest` with a new url): method, headers, cookies, and
 * body are kept and the page still sees its own origin.
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
    if (event.targetInfo.type === 'page' || event.targetInfo.type === 'iframe') {
      try {
        target.on('Fetch.requestPaused', (paused: RequestPaused) => {
          void forwardToDecoy(
            target,
            paused,
            decide(paused.request.url, decoyUrl, matchers, holder),
          )
        })
        await target.send('Fetch.enable', {
          patterns: [{ urlPattern: '*', requestStage: 'Request' }],
        })
        await autoAttach(target) // out-of-process iframes
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
      await session.send('Fetch.continueRequest', { requestId, url: decision.url, headers })
    }
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
