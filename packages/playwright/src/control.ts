import { SESSION_HEADER } from './routing'

/** The per-test handle to one Decoy Session. Every method calls the Control API immediately. */
export interface DecoyRouter {
  /** Session id stamped on this test's routed requests as `x-mock-session`. */
  readonly sessionId: string
  /** Switch this Session to another Collection. */
  useCollection(collection: string): Promise<void>
  /** Override one Behavior address (`route:case:behavior`) for this Session. */
  useRoute(route: string, caseId: string, behavior: string): Promise<void>
  /** Drop this Session's route overrides. */
  reset(): Promise<void>
  /** Free this Session in Decoy. The fixtures call it when the test ends. */
  endSession(): Promise<void>
}

/** A Router bound to one Session of the Decoy runtime at `url`. Errors reject at the call site. */
export function createRouter(url: string, sessionId: string): DecoyRouter {
  const control = async (verb: string, body?: object): Promise<void> => {
    const response = await fetch(`${url}/__decoy__/control/${verb}`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', [SESSION_HEADER]: sessionId },
      body: body === undefined ? undefined : JSON.stringify(body),
    })
    if (!response.ok) {
      throw new Error(`Decoy ${verb} failed (${response.status}): ${await errorMessage(response)}`)
    }
  }

  return {
    sessionId,
    useCollection: (collection) => control('useCollection', { collection }),
    useRoute: (route, caseId, behavior) => control('useRoute', { route, case: caseId, behavior }),
    reset: () => control('reset'),
    endSession: () => control('endSession'),
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
