import { randomUUID } from 'node:crypto'
import { createRouter, type DecoyRouter } from './control'
import { isHealthy } from './health'

/** A readable, unique Session id: `<title slug>-<uuid>`, so Decoy's logs point at the test. */
export function newSessionId(title: string): string {
  // Runs collapse to one '-', so at most one sits at each end; slicing beats a /^-+|-+$/ regex,
  // which backtracks polynomially on long runs of '-'.
  let readable = title.toLowerCase().replace(/[^a-z0-9]+/g, '-')
  readable = readable.slice(
    readable.startsWith('-') ? 1 : 0,
    readable.endsWith('-') ? -1 : undefined,
  )
  readable = readable.slice(0, 60)
  return readable ? `${readable}-${randomUUID()}` : randomUUID()
}

/**
 * A test's Session: check Decoy is reachable at `url`, then apply the default `collection`.
 * `startHint` ends the not-reachable error, telling the user how their runner starts Decoy.
 */
export async function openSession(
  url: string,
  sessionId: string,
  collection: string | undefined,
  startHint: string,
): Promise<DecoyRouter> {
  if (!(await isHealthy(url))) {
    throw new Error(`Decoy is not reachable at ${url}. ${startHint}`)
  }
  const router = createRouter(url, sessionId)
  if (collection !== undefined) {
    await router.useCollection(collection)
  }
  return router
}
