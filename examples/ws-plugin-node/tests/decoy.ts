import { type ChildProcess, spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import protobuf from 'protobufjs'

export const DECOY_URL = 'http://127.0.0.1:8095'
const exampleDir = fileURLToPath(new URL('..', import.meta.url))

/** The example's protos, to build and read payloads the way the app would. */
export const protos = protobuf.loadSync([
  `${exampleDir}proto/approvals/v1/approvals.proto`,
  `${exampleDir}proto/users/v1/users.proto`,
])

export function encodeProto(type: string, value: object): Uint8Array {
  const message = protos.lookupType(type)
  return message.encode(message.fromObject(value)).finish()
}

export function decodeProto(type: string, bytes: Uint8Array): unknown {
  const message = protos.lookupType(type)
  return message.toObject(message.decode(bytes), { enums: String })
}

/** Starts `decoy serve` for this example (or reuses one already serving); returns a stop function. */
export async function startDecoy(config = 'mocks/decoy.yaml'): Promise<() => void> {
  if (await healthy()) {
    return () => undefined
  }

  // DECOY_BINARY_PATH: a prebuilt `decoy` (as in CI); otherwise build and run it with cargo.
  const bin = process.env.DECOY_BINARY_PATH
  const serve = ['serve', '--config', config]
  const [command, ...args] = bin
    ? [bin, ...serve]
    : ['cargo', 'run', '--quiet', '-p', 'decoy', '--', ...serve]

  const child: ChildProcess = spawn(command, args, {
    cwd: exampleDir,
    // stderr carries Decoy's misses and the plugins' logs, prefixed `[rpc: binary-rpc@…]`.
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
    await new Promise((resolve) => setTimeout(resolve, 100))
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
export async function control(session: string, verb: string, body: object): Promise<Response> {
  return fetch(`${DECOY_URL}/__decoy__/control/${verb}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', 'x-mock-session': session },
    body: JSON.stringify(body),
  })
}

/** Pushes `message` to the Session's live sockets of `route`; returns how many got it. */
export async function send(session: string, route: string, message: unknown): Promise<number> {
  const response = await control(session, 'send', { route, message })
  if (!response.ok) {
    throw new Error(`send failed: ${response.status} ${await response.text()}`)
  }

  return ((await response.json()) as { delivered: number }).delivered
}

/** A frame as received: text, or the bytes of a binary frame. */
export type Frame = string | Uint8Array

/** A socket that queues what it receives, so a test can await the next frame in order. */
export class Socket {
  private readonly ws: WebSocket
  private readonly received: Frame[] = []
  private waiting: ((frame: Frame) => void) | undefined

  /** Opens `path` on Decoy, in `session`. */
  static async open(path: string, session: string): Promise<Socket> {
    // Node's WebSocket takes headers; a browser can't, which is what the Playwright bridge is for.
    const ws = new WebSocket(`${DECOY_URL.replace('http', 'ws')}${path}`, {
      headers: { 'x-mock-session': session },
    })
    ws.binaryType = 'arraybuffer'

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
      const frame: Frame =
        typeof event.data === 'string' ? event.data : new Uint8Array(event.data as ArrayBuffer)

      // Hand it to a waiting `next()`, or queue it for the next one.
      const waiting = this.waiting
      this.waiting = undefined
      if (waiting) {
        waiting(frame)
      } else {
        this.received.push(frame)
      }
    })
  }

  send(frame: Frame): void {
    this.ws.send(frame)
  }

  /** The next frame, within `timeout` ms. */
  next(timeout = 2000): Promise<Frame> {
    const queued = this.received.shift()
    if (queued !== undefined) {
      return Promise.resolve(queued)
    }

    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.waiting = undefined
        reject(new Error(`no frame within ${timeout}ms`))
      }, timeout)

      this.waiting = (frame) => {
        clearTimeout(timer)
        resolve(frame)
      }
    })
  }

  /** The next frame, which must be binary. */
  async binary(): Promise<Uint8Array> {
    const frame = await this.next()
    if (typeof frame === 'string') {
      throw new Error(`expected a binary frame, got text ${frame}`)
    }

    return frame
  }

  /** The next frame, which must be text holding JSON. */
  async json(): Promise<unknown> {
    const frame = await this.next()
    if (typeof frame !== 'string') {
      throw new Error(`expected a text frame, got ${frame.length} bytes`)
    }

    return JSON.parse(frame) as unknown
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
