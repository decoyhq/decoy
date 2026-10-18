// Starts Decoy, then runs the tests with NODE_EXTRA_CA_CERTS pointing at the CA that `tls: auto`
// writes. Node reads that variable only at startup, so the tests run in a child process.
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'

const exampleDir = fileURLToPath(new URL('..', import.meta.url))
export const CRM_URL = 'http://127.0.0.1:8092'

async function healthy(): Promise<boolean> {
  try {
    return (await fetch(`${CRM_URL}/__decoy__/health`)).ok
  } catch {
    return false
  }
}

let stopDecoy = (): void => undefined
if (!(await healthy())) {
  const decoy = spawn(
    'cargo',
    ['run', '--quiet', '-p', 'decoy-runtime', '--', 'serve', '--config', 'mocks/decoy.yaml'],
    { cwd: exampleDir, stdio: ['ignore', 'ignore', 'inherit'] },
  )
  stopDecoy = () => decoy.kill()
  // The first run compiles the decoy binary.
  const deadline = Date.now() + 180_000
  while (!(await healthy())) {
    if (decoy.exitCode !== null || Date.now() > deadline) {
      stopDecoy()
      throw new Error('decoy did not become healthy')
    }
    await new Promise((resolve) => setTimeout(resolve, 100))
  }
}

const tests = spawn(process.execPath, ['--test', 'tests/*.test.ts'], {
  cwd: exampleDir,
  stdio: 'inherit',
  env: { ...process.env, NODE_EXTRA_CA_CERTS: 'mocks/.decoy/ca.pem' },
})
tests.on('exit', (code) => {
  stopDecoy()
  process.exit(code ?? 1)
})
