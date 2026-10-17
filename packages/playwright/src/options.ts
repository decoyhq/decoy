/** Decoy options for a Playwright project, set under `use.decoyOptions`. */
export interface DecoyOptions {
  /** Base URL of the native Decoy runtime. Default `http://127.0.0.1:8080`. */
  url?: string
  /** Shell command `setup` runs when Decoy is not already serving. Default `decoy serve`. */
  command?: string
  /** Working directory for `command`. Default: the Playwright config file's directory. */
  cwd?: string
  /** Reuse a Decoy already serving at `url`. Default: `true` locally, `false` when `CI` is set. */
  reuseExistingServer?: boolean
  /** How long `setup` waits for health after starting `command`, in ms. Default 60000. */
  timeout?: number
}

export interface ResolvedDecoyOptions {
  url: string
  command: string
  cwd: string | undefined
  reuseExistingServer: boolean
  timeout: number
}

export const DEFAULT_URL = 'http://127.0.0.1:8080'
export const HEALTH_PATH = '/__decoy__/health'

export function resolveOptions(
  options: DecoyOptions,
  env: NodeJS.ProcessEnv = process.env,
): ResolvedDecoyOptions {
  return {
    url: (options.url ?? DEFAULT_URL).replace(/\/+$/, ''),
    command: options.command ?? 'decoy serve',
    cwd: options.cwd,
    reuseExistingServer: options.reuseExistingServer ?? !env.CI,
    timeout: options.timeout ?? 60_000,
  }
}

/** True when Decoy answers its liveness probe at `url`. */
export async function isHealthy(url: string): Promise<boolean> {
  try {
    const response = await fetch(`${url}${HEALTH_PATH}`, { signal: AbortSignal.timeout(1_000) })
    return response.ok
  } catch {
    return false
  }
}
