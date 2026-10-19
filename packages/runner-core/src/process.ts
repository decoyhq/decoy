import { type ChildProcess, spawn } from 'node:child_process'
import { existsSync } from 'node:fs'
import { delimiter, dirname, join } from 'node:path'

/**
 * `env` with every `node_modules/.bin` from `cwd` up to the root put first on PATH, as npm scripts
 * do, so `decoy` from `@decoyhq/cli` is found even when the test runner isn't started by a script.
 */
export function withLocalBins(cwd: string, env: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  const bins: string[] = []
  for (let dir = cwd; ; dir = dirname(dir)) {
    const bin = join(dir, 'node_modules', '.bin')
    if (existsSync(bin)) {
      bins.push(bin)
    }
    if (dirname(dir) === dir) {
      break
    }
  }
  // Windows spells it `Path`; keep whichever key the environment uses.
  const key = Object.keys(env).find((name) => name.toUpperCase() === 'PATH') ?? 'PATH'
  return { ...env, [key]: [...bins, env[key]].filter(Boolean).join(delimiter) }
}

/**
 * Stops `child` and everything it started: SIGTERM to its process group (Decoy drains open
 * requests for up to 5s), then SIGKILL after `graceMs`. Never throws for a process that is gone.
 */
export async function stopProcess(child: ChildProcess, graceMs = 8_000): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null || child.pid === undefined) {
    return
  }
  const pid = child.pid
  const exited = new Promise<boolean>((resolve) => {
    child.once('exit', () => {
      resolve(true)
    })
  })
  const within = (ms: number) =>
    Promise.race([
      exited,
      new Promise<boolean>((resolve) =>
        // unref: an already-exited Decoy mustn't keep the caller alive for the grace period.
        setTimeout(() => {
          resolve(false)
        }, ms).unref(),
      ),
    ])

  if (process.platform === 'win32') {
    spawn('taskkill', ['/pid', String(pid), '/T', '/F']).on('error', () => undefined)
    await within(graceMs)
    return
  }
  // The shell runs in its own process group (detached), so this also reaches `decoy` under it.
  signalGroup(pid, 'SIGTERM')
  if (!(await within(graceMs))) {
    signalGroup(pid, 'SIGKILL')
    await within(2_000)
  }
}

/**
 * Stops the process group led by `pid`, which is not our child (an earlier run started it), so
 * there is no exit event: SIGTERM, then SIGKILL if it outlives `graceMs`.
 */
export async function stopGroup(pid: number, graceMs = 8_000): Promise<void> {
  if (process.platform === 'win32') {
    await new Promise((done) => {
      spawn('taskkill', ['/pid', String(pid), '/T', '/F'])
        .on('error', done)
        .on('exit', done)
    })
    return
  }
  signalGroup(pid, 'SIGTERM')
  if (!(await goneWithin(pid, graceMs))) {
    signalGroup(pid, 'SIGKILL')
    await goneWithin(pid, 2_000)
  }
}

/** True while the process group led by `pid` (on Windows: the process) still exists. */
export function isAlive(pid: number): boolean {
  try {
    process.kill(process.platform === 'win32' ? pid : -pid, 0)
    return true
  } catch {
    return false
  }
}

async function goneWithin(pid: number, ms: number): Promise<boolean> {
  for (const deadline = Date.now() + ms; Date.now() < deadline;) {
    if (!isAlive(pid)) {
      return true
    }
    await new Promise((resolve) => setTimeout(resolve, 50))
  }
  return !isAlive(pid)
}

function signalGroup(pid: number, signal: NodeJS.Signals): void {
  try {
    process.kill(-pid, signal)
  } catch {
    // ESRCH: the group is already gone.
  }
}
