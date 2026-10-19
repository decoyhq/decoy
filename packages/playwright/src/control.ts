import { SESSION_HEADER } from './routing'

/** The per-test handle to one Decoy Session. Every method calls the Control API immediately. */
export interface DecoyRouter {
  /** Session id stamped on this test's routed requests as `x-mock-session`. */
  readonly sessionId: string
  /** Switch this Session to another Collection. */
  useCollection(collection: string): Promise<void>
  /** Override one Behavior address (`route:case:behavior`) for this Session. */
  useRoute(address: string): Promise<void>
  useRoute(route: string, caseId: string, behavior: string): Promise<void>
  /** Drop this Session's route overrides. */
  reset(): Promise<void>
  /** End this Session in Decoy (gives up after 2s). The fixtures call it when the test ends. */
  endSession(): Promise<void>
}

/**
 * A Router bound to one Session of the Decoy runtime at `url`. Errors reject at the call site; a
 * call Decoy does not answer within `timeoutMs` rejects too.
 */
export function createRouter(url: string, sessionId: string, timeoutMs = 10_000): DecoyRouter {
  const control = async (
    verb: string,
    body?: object,
    signal = AbortSignal.timeout(timeoutMs),
  ): Promise<void> => {
    let response: Response
    try {
      response = await fetch(`${url}/__decoy__/control/${verb}`, {
        method: 'POST',
        headers: { 'content-type': 'application/json', [SESSION_HEADER]: sessionId },
        body: body === undefined ? undefined : JSON.stringify(body),
        signal,
      })
    } catch (error) {
      throw new Error(`Decoy ${verb} at ${url} failed: ${String(error)}`, { cause: error })
    }
    if (!response.ok) {
      throw new Error(`Decoy ${verb} failed (${response.status}): ${await errorMessage(response)}`)
    }
  }

  return {
    sessionId,
    useCollection: (collection) => control('useCollection', { collection }),
    useRoute: async (route: string, caseId?: string, behavior?: string) => {
      if (caseId === undefined || behavior === undefined) {
        const parts = route.split(':')
        if (parts.length !== 3 || parts.some((part) => part === '')) {
          throw new Error(`Invalid Behavior address \`${route}\`: expected \`route:case:behavior\``)
        }
        ;[route, caseId, behavior] = parts as [string, string, string]
      }
      await control('useRoute', { route, case: caseId, behavior })
    },
    reset: () => control('reset'),
    // Runs in test teardown: an unreachable Decoy must not turn a passed test into a timeout.
    endSession: () => control('endSession', undefined, AbortSignal.timeout(2000)),
  }
}

async function errorMessage(response: Response): Promise<string> {
  const text = await response.text()
  try {
    const parsed: unknown = JSON.parse(text)
    if (typeof parsed === 'object' && parsed !== null && 'error' in parsed) {
      return String(parsed.error)
    }
  } catch {
    // Not the Control API error envelope; report the raw body.
  }
  return text
}
