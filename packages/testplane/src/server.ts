import { type ChildProcess, spawn } from 'node:child_process'
import { resolve } from 'node:path'
import { type DecoyOptions, isHealthy, type ResolvedDecoyOptions, resolveOptions } from './options'
import { stopProcess, withLocalBins } from './process'

export type Teardown = () => Promise<void>

/**
 * Start `command` unless Decoy already answers at `url` (reuse follows Playwright `webServer`:
 * allowed locally, refused when `CI` is set). The teardown stops Decoy only if this call started it.
 */
export async function startDecoy(decoyOptions: DecoyOptions): Promise<Teardown> {
  const options = resolveOptions(decoyOptions)
  const cwd = resolve(options.cwd)

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
  try {
    await waitForHealth(child, options)
  } catch (error) {
    await stopProcess(child)
    throw error
  }
  return () => stopProcess(child)
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
