export const DEFAULT_URL = 'http://127.0.0.1:8080'
export const HEALTH_PATH = '/__decoy__/health'

/** True when Decoy answers its liveness probe at `url`. */
export async function isHealthy(url: string): Promise<boolean> {
  try {
    const response = await fetch(`${url}${HEALTH_PATH}`, { signal: AbortSignal.timeout(1_000) })
    return response.ok
  } catch {
    return false
  }
}
