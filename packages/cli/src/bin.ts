import { spawn } from 'node:child_process'
import { createRequire } from 'node:module'
import { findBinary } from './binary'

let binary: string
try {
  binary = findBinary({
    platform: process.platform,
    arch: process.arch,
    env: process.env,
    resolve: createRequire(import.meta.url).resolve,
  })
} catch (error) {
  console.error(error instanceof Error ? error.message : error)
  process.exit(1)
}

const child = spawn(binary, process.argv.slice(2), { stdio: 'inherit' })
for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP'] as const) {
  process.on(signal, () => child.kill(signal))
}
child.on('error', (error) => {
  console.error(`Failed to run ${binary}: ${error.message}`)
  process.exit(1)
})
child.on('exit', (code, signal) => {
  if (signal) {
    // Die the same way the binary did; drop our forwarder first so the signal is not swallowed.
    process.removeAllListeners(signal)
    process.kill(process.pid, signal)
  } else {
    process.exit(code ?? 1)
  }
})
