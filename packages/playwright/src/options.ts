import type { ServerOptions } from '@decoyhq/runner-core'

/** Decoy options for a Playwright project, set under `use.decoyOptions`. */
export interface DecoyOptions extends ServerOptions {
  /**
   * Browser request URL patterns (Playwright `context.route` globs or RegExps) to send to Decoy.
   * Matching requests always go to Decoy; everything else continues normally.
   */
  urls?: string | RegExp | ReadonlyArray<string | RegExp>
  /** Collection each test's Session starts on, applied before any routed request. */
  collection?: string
}
