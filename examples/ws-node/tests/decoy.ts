import { spawn } from 'node:child_process'
import type { ChildProcess } from 'node:child_process'
import { fileURLToPath } from 'node:url'

export const DECOY_URL = 'http://127.0.0.1:8094'
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

/** Pushes `message` to the Session's live sockets of `route`; returns how many got it. */
export async function send(session: string, route: string, message: unknown): Promise<number> {
  const response = await fetch(`${DECOY_URL}/__decoy__/control/send`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', 'x-mock-session': session },
    body: JSON.stringify({ route, message }),
  })
  if (!response.ok) {
    throw new Error(`send failed: ${response.status} ${await response.text()}`)
  }
  return ((await response.json()) as { delivered: number }).delivered
}

/** A socket that queues what it receives, so a test can await the next message in order. */
export class Socket {
  readonly closed: Promise<CloseEvent>
  private readonly ws: WebSocket
  private readonly received: string[] = []
  private waiting: ((text: string) => void) | undefined

  /** Opens `/subscribe?client=browser&service=<service>`, in `session` when given. */
  static async open(service: string, session?: string): Promise<Socket> {
    const url = `${DECOY_URL.replace('http', 'ws')}/subscribe?client=browser&service=${service}`
    // Node's WebSocket takes headers; a browser can't, which is what the Playwright bridge is for.
    const ws = new WebSocket(url, session ? { headers: { 'x-mock-session': session } } : {})
    const socket = new Socket(ws)
    await new Promise((resolve, reject) => {
      ws.addEventListener('open', resolve, { once: true })
      ws.addEventListener('error', reject, { once: true })
    })
    return socket
  }

  private constructor(ws: WebSocket) {
    this.ws = ws
    ws.addEventListener('message', (event) => {
      const text = String(event.data)
      const waiting = this.waiting
      this.waiting = undefined
      if (waiting) {
        waiting(text)
      } else {
        this.received.push(text)
      }
    })
    this.closed = new Promise((resolve) => {
      ws.addEventListener('close', resolve, { once: true })
    })
  }

  send(message: object): void {
    this.ws.send(JSON.stringify(message))
  }

  /** The next text frame, within `timeout` ms. */
  next(timeout = 2000): Promise<string> {
    const queued = this.received.shift()
    if (queued !== undefined) {
      return Promise.resolve(queued)
    }
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.waiting = undefined
        reject(new Error(`no message within ${timeout}ms`))
      }, timeout)
      this.waiting = (text) => {
        clearTimeout(timer)
        resolve(text)
      }
    })
  }

  async nextJson(): Promise<unknown> {
    return JSON.parse(await this.next()) as unknown
  }

  /** Resolves when nothing arrives for `ms`. */
  async silent(ms = 300): Promise<boolean> {
    return this.next(ms).then(
      () => false,
      () => true,
    )
  }

  close(): void {
    this.ws.close()
  }
}
