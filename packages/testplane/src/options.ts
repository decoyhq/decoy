/** Options for the `@decoyhq/testplane` plugin, set under `plugins` in the Testplane config. */
export interface DecoyOptions {
  /** Turn the plugin off without removing it. Default `true`. */
  enabled?: boolean
  /** Base URL of the native Decoy runtime. Default `http://127.0.0.1:8080`. */
  url?: string
  /** Shell command the plugin runs when Decoy is not already serving. Default `decoy serve`. */
  command?: string
  /** Working directory for `command`. Default: `process.cwd()`. */
  cwd?: string
  /** Reuse a Decoy already serving at `url`. Default: `true` locally, `false` when `CI` is set. */
  reuseExistingServer?: boolean
  /** How long the plugin waits for health after starting `command`, in ms. Default 60000. */
  timeout?: number
  /**
   * Browser request URL patterns to send to Decoy: globs (`*` within a path segment, `**` across
   * them) matched against the full URL, or RegExps. Everything else continues normally.
   */
  urls?: string | RegExp | ReadonlyArray<string | RegExp>
  /** Collection each test's Session starts on, applied before the test runs. */
  collection?: string
}

export interface ResolvedDecoyOptions {
  url: string
  command: string
  cwd: string
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
    url: trimTrailingSlashes(options.url ?? DEFAULT_URL),
    command: options.command ?? 'decoy serve',
    cwd: options.cwd ?? process.cwd(),
    reuseExistingServer: options.reuseExistingServer ?? !env.CI,
    timeout: options.timeout ?? 60_000,
  }
}

// A loop, not /\/+$/: that regex backtracks polynomially on long runs of '/'.
function trimTrailingSlashes(url: string): string {
  let end = url.length
  while (end > 0 && url[end - 1] === '/') {
    end--
  }
  return url.slice(0, end)
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
