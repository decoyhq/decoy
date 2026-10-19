/** Options for the `@decoyhq/testplane` plugin, set under `plugins` in the Testplane config. */
export interface DecoyOptions {
  /** Turn the plugin off without removing it. Default `true`. */
  enabled?: boolean
  /** Base URL of the native Decoy runtime. Default `http://127.0.0.1:8080`. */
  url?: string
  /** Shell command the plugin runs when Decoy is not already serving. Default `decoy serve`. */
  command?: string
  /** Working directory for `command` (relative: to `process.cwd()`). Default: the config file's directory. */
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
