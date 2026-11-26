import { spawn } from 'node:child_process'
import type { ChildProcess } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { credentials, loadPackageDefinition, Metadata } from '@grpc/grpc-js'
import type { Client, ServiceClientConstructor, ServiceError } from '@grpc/grpc-js'
import { loadSync } from '@grpc/proto-loader'

export const DECOY_URL = 'http://127.0.0.1:8091'
const exampleDir = fileURLToPath(new URL('..', import.meta.url))

/** Starts `decoy serve` for this example (or reuses one already serving); returns a stop function. */
export async function startDecoy(): Promise<() => void> {
  if (await healthy()) {
    return () => undefined
  }
  // DECOY_BINARY_PATH: a prebuilt `decoy` (as in CI); otherwise build and run it with cargo.
  const bin = process.env.DECOY_BINARY_PATH
  const serve = ['serve', '--config', 'mocks/decoy.yaml']
  const [command, ...args] = bin
    ? [bin, ...serve]
    : ['cargo', 'run', '--quiet', '-p', 'decoy', '--', ...serve]
  const child: ChildProcess = spawn(command, args, {
    cwd: exampleDir,
    stdio: ['ignore', 'ignore', 'inherit'],
  })
  // The first run compiles the decoy binary.
  const deadline = Date.now() + 180_000
  while (!(await healthy())) {
    if (child.exitCode !== null) {
      throw new Error(`decoy serve exited with code ${child.exitCode}`)
    }
    if (Date.now() > deadline) {
      child.kill()
      throw new Error('decoy did not become healthy')
    }
    await new Promise((resolve) => {
      setTimeout(resolve, 100)
    })
  }
  return () => child.kill()
}

async function healthy(): Promise<boolean> {
  try {
    return (await fetch(`${DECOY_URL}/__decoy__/health`)).ok
  } catch {
    return false
  }
}

/** Calls the Control API for one Session, as the Playwright fixture does. */
export async function control(session: string, verb: string, body: object): Promise<void> {
  const response = await fetch(`${DECOY_URL}/__decoy__/control/${verb}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', 'x-mock-session': session },
    body: JSON.stringify(body),
  })
  if (!response.ok) {
    throw new Error(`${verb} failed: ${response.status} ${await response.text()}`)
  }
}

export interface Approval {
  id: string
  state: string
  approvers: string[]
}

type Callback = (error: ServiceError | null, reply?: Approval) => void
interface ApprovalsClient extends Client {
  getApproval(request: { id: string }, metadata: Metadata, callback: Callback): void
}

const definition = loadSync('approvals/v1/approvals.proto', {
  includeDirs: [fileURLToPath(new URL('../proto', import.meta.url))],
  enums: String,
  defaults: true,
})
interface ApprovalsPackage {
  approvals: { v1: { Approvals: ServiceClientConstructor } }
}
const { Approvals } = (loadPackageDefinition(definition) as unknown as ApprovalsPackage).approvals
  .v1

/** A plain `@grpc/grpc-js` client pointed at Decoy instead of the real Approvals service. */
export function approvalsClient(): ApprovalsClient {
  return new Approvals(
    DECOY_URL.replace('http://', ''),
    credentials.createInsecure(),
  ) as unknown as ApprovalsClient
}

/** One unary call; `session` and other metadata ride along as gRPC metadata. */
export function getApproval(
  client: ApprovalsClient,
  id: string,
  metadata: Record<string, string> = {},
): Promise<Approval> {
  const meta = new Metadata()
  for (const [key, value] of Object.entries(metadata)) {
    meta.set(key, value)
  }
  return new Promise((resolve, reject) => {
    client.getApproval({ id }, meta, (error, reply) => {
      if (reply) {
        resolve(reply)
      } else {
        reject(error ?? new Error('empty reply'))
      }
    })
  })
}
