import { type ChildProcess, spawn } from 'node:child_process'
import { readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { DEFAULT_URL, isHealthy } from './health'
import { isAlive, stopGroup, stopProcess, withLocalBins } from './process'

export type Teardown = () => Promise<void>

/** How to reach or start Decoy; every integration's options extend these. */
export interface ServerOptions {
  /** Base URL of the native Decoy runtime. Default `http://127.0.0.1:8080`. */
  url?: string
  /** Shell command run when Decoy is not already serving. Default `decoy serve`. */
  command?: string
  /** Working directory for `command` (relative: to `process.cwd()`). Default: the config file's directory. */
  cwd?: string
  /** Reuse a Decoy already serving at `url`. Default: `true` locally, `false` when `CI` is set. */
  reuseExistingServer?: boolean
  /** How long to wait for health after starting `command`, in ms. Default 60000. */
  timeout?: number
}

export interface ResolvedServerOptions {
  url: string
  command: string
  cwd: string | undefined
  reuseExistingServer: boolean
  timeout: number
}

export function resolveServerOptions(
  options: ServerOptions,
  env: NodeJS.ProcessEnv = process.env,
): ResolvedServerOptions {
  return {
    url: trimTrailingSlashes(options.url ?? DEFAULT_URL),
    command: options.command ?? 'decoy serve',
    cwd: options.cwd,
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

/**
 * Start `command` in `cwd` unless Decoy already answers at `url`. Reuse follows Playwright
 * `webServer`: allowed locally, refused when `CI` is set. The teardown stops Decoy only if this
 * call started it.
 */
export async function startDecoy(
  options: ResolvedServerOptions & { cwd: string },
): Promise<Teardown> {
  // Absolute, so the PATH entries and the walk up to the root don't depend on process.cwd().
  const cwd = resolve(options.cwd)

  const pidFile = join(tmpdir(), `decoy-runner-${new URL(options.url).port || '80'}.json`)
  await stopOrphan(pidFile, options, cwd)

  if (await isHealthy(options.url)) {
    if (options.reuseExistingServer) {
      return () => Promise.resolve()
    }
    throw new Error(
      `Decoy is already serving at ${options.url}. Stop it, or set reuseExistingServer to true.`,
    )
  }

  const child = spawn(options.command, {
    cwd,
    env: withLocalBins(cwd, process.env),
    shell: true,
    detached: process.platform !== 'win32',
    stdio: ['ignore', 'ignore', 'inherit'],
  })
  let wrotePidFile = false
  try {
    // Written before health passes, so a runner interrupted mid-start still leaves a trail.
    writeFileSync(
      pidFile,
      JSON.stringify({ pid: child.pid, runner: process.pid, cwd, command: options.command }),
    )
    wrotePidFile = true
    await waitForHealth(child, options)
  } catch (error) {
    await stopProcess(child)
    // A pidfile this start couldn't write is another runner's: leave it.
    if (wrotePidFile) {
      removePidFile(pidFile)
    }
    throw error
  }
  return async () => {
    await stopProcess(child)
    removePidFile(pidFile)
  }
}

function removePidFile(pidFile: string) {
  try {
    rmSync(pidFile, { force: true })
  } catch {
    // One that can't be removed (another user's, in a shared tmpdir) must not mask the real error.
  }
}

/**
 * Stops a Decoy that an earlier `startDecoy` spawned with the same command and cwd but never tore
 * down (its runner was hard-killed), so it isn't silently reused. A Decoy the user started has no
 * pidfile, and one whose runner is still running isn't an orphan; both are left alone.
 */
async function stopOrphan(pidFile: string, options: ResolvedServerOptions, cwd: string) {
  let recorded: { pid: number; runner: number; cwd: string; command: string }
  try {
    recorded = JSON.parse(readFileSync(pidFile, 'utf8')) as typeof recorded
  } catch {
    return
  }
  if (processExists(recorded.runner)) {
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
  removePidFile(pidFile)
}

async function waitForHealth(child: ChildProcess, options: ResolvedServerOptions): Promise<void> {
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

function processExists(pid: number): boolean {
  try {
    process.kill(pid, 0)
    return true
  } catch {
    return false
  }
}
