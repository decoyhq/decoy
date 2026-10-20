import { dirname } from 'node:path'
import { resolveServerOptions, startDecoy, type Teardown } from '@decoyhq/runner-core'
import type { FullConfig, FullProject } from '@playwright/test'
import type { DecoyTestOptions } from './fixtures'
import type { DecoyOptions } from './options'

export type { Teardown }

const keepRunning: Teardown = () => Promise.resolve()

/** The part of Playwright's `FullConfig<DecoyTestOptions>` that `setup` reads. */
export type SetupConfig = Pick<FullConfig<DecoyTestOptions>, 'configFile'> & {
  projects: ReadonlyArray<Pick<FullProject<DecoyTestOptions>, 'use'>>
}

/**
 * Playwright global setup: start or reuse native Decoy, following Playwright `webServer` reuse
 * semantics, once per distinct `decoyOptions.url` across projects. Returns a teardown that stops
 * only the Decoys this call started.
 *
 * ```ts
 * // global-setup.ts
 * export { setup as default } from '@decoyhq/playwright'
 * ```
 */
export async function setup(config: SetupConfig): Promise<Teardown> {
  const teardowns: Teardown[] = []
  const stopAll: Teardown = async () => {
    await Promise.all(teardowns.splice(0).map((teardown) => teardown()))
  }
  try {
    for (const decoyOptions of distinctDecoyOptions(config)) {
      teardowns.push(await start(decoyOptions, config))
    }
  } catch (error) {
    await stopAll()
    throw error
  }
  return teardowns.length === 0 ? keepRunning : stopAll
}

function start(decoyOptions: DecoyOptions, config: SetupConfig): Promise<Teardown> {
  const options = resolveServerOptions(decoyOptions)
  const cwd = options.cwd ?? (config.configFile ? dirname(config.configFile) : process.cwd())
  return startDecoy({ ...options, cwd })
}

/**
 * One set of options per Decoy `url`: projects sharing a url share one Decoy, so they must agree
 * on how it starts.
 */
function distinctDecoyOptions(config: SetupConfig): DecoyOptions[] {
  const byUrl = new Map<string, DecoyOptions>()
  for (const { use } of config.projects) {
    if (!use.decoyOptions) {
      continue
    }
    const { url } = resolveServerOptions(use.decoyOptions)
    const first = byUrl.get(url)
    if (first === undefined) {
      byUrl.set(url, use.decoyOptions)
    } else if (!sameStart(first, use.decoyOptions)) {
      throw new Error(
        `Projects share the Decoy at ${url} but set different command, cwd, reuseExistingServer or timeout; give them one set of decoyOptions or different urls.`,
      )
    }
  }
  return [...byUrl.values()]
}

function sameStart(a: DecoyOptions, b: DecoyOptions): boolean {
  const left = resolveServerOptions(a)
  const right = resolveServerOptions(b)
  return (
    left.command === right.command &&
    left.cwd === right.cwd &&
    left.reuseExistingServer === right.reuseExistingServer &&
    left.timeout === right.timeout
  )
}
