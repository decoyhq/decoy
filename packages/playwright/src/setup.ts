import { type ChildProcess, spawn } from 'node:child_process'
import { readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import type { FullConfig, FullProject } from '@playwright/test'
import type { DecoyTestOptions } from './fixtures'
import { type DecoyOptions, isHealthy, type ResolvedDecoyOptions, resolveOptions } from './options'
import { isAlive, stopGroup, stopProcess, withLocalBins } from './process'

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

async function start(decoyOptions: DecoyOptions, config: SetupConfig): Promise<Teardown> {
  const options = resolveOptions(decoyOptions)
  // Absolute, so the PATH entries and the walk up to the root don't depend on process.cwd().
  const cwd = resolve(
    options.cwd ?? (config.configFile ? dirname(config.configFile) : process.cwd()),
  )

  const pidFile = join(tmpdir(), `decoy-playwright-${new URL(options.url).port || '80'}.json`)
  await stopOrphan(pidFile, options, cwd)

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
  writeFileSync(pidFile, JSON.stringify({ pid: child.pid, cwd, command: options.command }))
  return async () => {
    await stopProcess(child)
    rmSync(pidFile, { force: true })
  }
}

/**
 * Stops a Decoy that an earlier `setup` spawned with the same command and cwd but never tore down
 * (its runner was hard-killed), so it isn't silently reused. A Decoy the user started has no
 * pidfile and is left alone.
 */
async function stopOrphan(pidFile: string, options: ResolvedDecoyOptions, cwd: string) {
  let recorded: { pid: number; cwd: string; command: string }
  try {
    recorded = JSON.parse(readFileSync(pidFile, 'utf8')) as typeof recorded
  } catch {
    return
  }
  // shortcut: a recycled pid is told apart only by Decoy still serving at url, check the process's
  // own command line if that ever kills the wrong group.
  if (isAlive(recorded.pid)) {
    if (
      recorded.command !== options.command ||
      recorded.cwd !== cwd ||
      !(await isHealthy(options.url))
    ) {
      return
    }
    await stopGroup(recorded.pid)
  }
  rmSync(pidFile, { force: true })
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
    const { url } = resolveOptions(use.decoyOptions)
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
  const left = resolveOptions(a)
  const right = resolveOptions(b)
  return (
    left.command === right.command &&
    left.cwd === right.cwd &&
    left.reuseExistingServer === right.reuseExistingServer &&
    left.timeout === right.timeout
  )
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
