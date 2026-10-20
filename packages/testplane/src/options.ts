import type { ServerOptions } from '@decoyhq/runner-core'

/** Options for the `@decoyhq/testplane` plugin, set under `plugins` in the Testplane config. */
export interface DecoyOptions extends ServerOptions {
  /** Turn the plugin off without removing it. Default `true`. */
  enabled?: boolean
  /**
   * Browser request URL patterns to send to Decoy: globs (`*` within a path segment, `**` across
   * them) matched against the full URL, or RegExps. Everything else continues normally.
   */
  urls?: string | RegExp | ReadonlyArray<string | RegExp>
  /** Collection each test's Session starts on, applied before the test runs. */
  collection?: string
}
