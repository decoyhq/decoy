/** Decoy options for a Playwright project, set under `use.decoyOptions`. */
export interface DecoyOptions {
  /** Base URL of the native Decoy runtime. Default `http://127.0.0.1:8080`. */
  url?: string
  /** Shell command `setup` runs when Decoy is not already serving. Default `decoy serve`. */
  command?: string
  /** Working directory for `command` (relative: to `process.cwd()`). Default: the config file's directory. */
  cwd?: string
  /** Reuse a Decoy already serving at `url`. Default: `true` locally, `false` when `CI` is set. */
  reuseExistingServer?: boolean
  /** How long `setup` waits for health after starting `command`, in ms. Default 60000. */
  timeout?: number
  /**
   * Browser request URL patterns (Playwright `context.route` globs or RegExps) to send to Decoy.
   * Matching requests always go to Decoy; everything else continues normally.
   */
  urls?: string | RegExp | ReadonlyArray<string | RegExp>
  /** Collection each test's Session starts on, applied before any routed request. */
  collection?: string
}

export {
  DEFAULT_URL,
  isHealthy,
  resolveServerOptions as resolveOptions,
} from '@decoyhq/runner-core'
