import { type ChildProcess, spawn } from 'node:child_process'
import { dirname } from 'node:path'
import type { FullConfig, FullProject } from '@playwright/test'
import type { DecoyTestOptions } from './fixtures'
import { type DecoyOptions, isHealthy, type ResolvedDecoyOptions, resolveOptions } from './options'
import { stopProcess, withLocalBins } from './process'

export type Teardown = () => Promise<void>

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
    for (const teardown of teardowns.splice(0).reverse()) {
      await teardown()
    }
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

async function start(decoyOptions: DecoyOptions, config: SetupConfig): Promise<Teardown> {
  const options = resolveOptions(decoyOptions)
  const cwd = options.cwd ?? (config.configFile ? dirname(config.configFile) : process.cwd())

  if (await isHealthy(options.url)) {
    if (options.reuseExistingServer) {
      return keepRunning
    }
    throw new Error(
      `Decoy is already serving at ${options.url}. Stop it, or set decoyOptions.reuseExistingServer to true.`,
    )
  }

  const child = spawn(options.command, {
    cwd,
    env: withLocalBins(cwd, process.env),
    shell: true,
    detached: process.platform !== 'win32',
    stdio: ['ignore', 'ignore', 'inherit'],
  })
  try {
    await waitForHealth(child, options)
  } catch (error) {
    await stopProcess(child)
    throw error
  }
  return () => stopProcess(child)
}

/** The first project's options for each Decoy `url`; projects sharing a url share one Decoy. */
function distinctDecoyOptions(config: SetupConfig): DecoyOptions[] {
  const byUrl = new Map<string, DecoyOptions>()
  for (const { use } of config.projects) {
    if (use.decoyOptions) {
      const { url } = resolveOptions(use.decoyOptions)
      if (!byUrl.has(url)) {
        byUrl.set(url, use.decoyOptions)
      }
    }
  }
  return [...byUrl.values()]
}

async function waitForHealth(child: ChildProcess, options: ResolvedDecoyOptions): Promise<void> {
  let exit: string | undefined
  child.once('exit', (code, signal) => {
    exit = signal ? `signal ${signal}` : `code ${code}`
  })
  child.once('error', (error) => {
    exit = error.message
  })

  const deadline = Date.now() + options.timeout
  while (Date.now() < deadline) {
    if (exit !== undefined) {
      throw new Error(`Decoy command \`${options.command}\` exited before serving (${exit}).`)
    }
    if (await isHealthy(options.url)) {
      return
    }
    await new Promise((resolve) => setTimeout(resolve, 100))
  }
  throw new Error(
    `Decoy did not become healthy at ${options.url} within ${options.timeout}ms (command: \`${options.command}\`).`,
  )
}
