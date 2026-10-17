import { type ChildProcess, spawn } from 'node:child_process'
import { dirname } from 'node:path'
import type { FullConfig } from '@playwright/test'
import { type DecoyOptions, isHealthy, type ResolvedDecoyOptions, resolveOptions } from './options'

export type Teardown = () => Promise<void>

/**
 * Playwright global setup: start or reuse native Decoy, following Playwright `webServer` reuse
 * semantics. Returns a teardown that stops Decoy only if this call started it.
 *
 * ```ts
 * // global-setup.ts
 * export { setup as default } from '@decoy/playwright'
 * ```
 */
export async function setup(config: FullConfig): Promise<Teardown> {
  const decoyOptions = findDecoyOptions(config)
  if (!decoyOptions) {
    return async () => {}
  }
  const options = resolveOptions(decoyOptions)
  const cwd = options.cwd ?? (config.configFile ? dirname(config.configFile) : undefined)

  if (await isHealthy(options.url)) {
    if (options.reuseExistingServer) {
      return async () => {}
    }
    throw new Error(
      `Decoy is already serving at ${options.url}. Stop it, or set decoyOptions.reuseExistingServer to true.`,
    )
  }

  const child = spawn(options.command, {
    cwd,
    shell: true,
    detached: process.platform !== 'win32',
    stdio: ['ignore', 'ignore', 'inherit'],
  })
  try {
    await waitForHealth(child, options)
  } catch (error) {
    await stop(child)
    throw error
  }
  return () => stop(child)
}

function findDecoyOptions(config: FullConfig): DecoyOptions | undefined {
  for (const project of config.projects) {
    const options = (project.use as { decoyOptions?: DecoyOptions }).decoyOptions
    if (options) {
      return options
    }
  }
  return undefined
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

async function stop(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null || child.pid === undefined) {
    return
  }
  const exited = new Promise<void>((resolve) => child.once('exit', () => resolve()))
  if (process.platform === 'win32') {
    spawn('taskkill', ['/pid', String(child.pid), '/T', '/F'])
  } else {
    // The shell runs in its own process group (detached), so this also stops `decoy` under it.
    process.kill(-child.pid, 'SIGTERM')
  }
  await exited
}
