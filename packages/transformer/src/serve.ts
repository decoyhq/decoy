import type { Readable, Writable } from 'node:stream'
import { type Connection, type Context, type Json, Miss, PROTOCOL, type Transformer } from './index'
import { read, type WireMessage, write } from './wire'

interface Io {
  input: Readable
  output: Writable

  /** Called once Decoy asks the worker to stop, or stops talking to it. */
  exit: (code: number) => void
}

const processIo: Io = {
  input: process.stdin,
  output: process.stdout,
  exit: (code) => process.exit(code),
}

interface HostCall {
  resolve: (value: unknown) => void
  reject: (error: Error) => void
}

function runOperation(transformer: Transformer, message: WireMessage, ctx: Context) {
  switch (message.op) {
    case 'input':
    case 'output': {
      const transform = transformer[message.op]
      if (!transform) {
        throw new Error(`Transformer worker ${transformer.name} does not provide ${message.op}`)
      }
      return transform(message.input, ctx)
    }
    case 'check':
      return transformer.check?.(message.input, ctx)
    default:
      throw new Error(`unknown operation ${String(message.op)}`)
  }
}

/** Correlates protobuf host calls with Decoy's replies. */
function createHostCalls(send: (message: WireMessage) => void) {
  const hostCalls = new Map<number, HostCall>()
  let nextHostId = 1

  const host = (op: string, args: unknown[]) =>
    new Promise<unknown>((resolve, reject) => {
      const id = nextHostId++
      hostCalls.set(id, { resolve, reject })
      try {
        send({ kind: 'host', id, op, args })
      } catch (error) {
        hostCalls.delete(id)
        reject(error instanceof Error ? error : new Error(String(error)))
      }
    })

  // Decoy's compiled protos, reached through host calls.
  const protobuf: Context['protobuf'] = {
    has: async (type) => (await host('protobuf.has', [type])) as boolean,
    decode: async (type, bytes) => (await host('protobuf.decode', [type, bytes])) as Json,
    encode: async (type, value) => (await host('protobuf.encode', [type, value])) as Uint8Array,
  }

  const settle = (message: WireMessage) => {
    const id = message.id as number
    const pending = hostCalls.get(id)
    if (!pending) return
    hostCalls.delete(id)

    if (message.ok) pending.resolve(message.value)
    else pending.reject(new Error(String(message.error)))
  }

  return { protobuf, settle }
}

/** Answers Decoy's messages on `io` for `transformer` (`docs/reference/worker-protocol.md`). */
export function serve(transformer: Transformer, io: Io = processIo): void {
  const send = (message: WireMessage) => {
    write(io.output, message)
  }
  const { protobuf, settle } = createHostCalls(send)

  // `ctx.state` of each open connection, by connection id.
  const states = new Map<string, Map<string, unknown>>()
  const context = (message: WireMessage): Context => {
    const connection = message.connection as Connection | undefined

    // Calls without a connection get a fresh state.
    let state = connection && states.get(connection.id)
    if (!state) {
      state = new Map()
      if (connection) states.set(connection.id, state)
    }

    const frame = message.frame as Context['frame']
    const request = message.request as Context['request']
    return { frame, connection, state, protobuf, ...(request && { request }) }
  }

  const call = async (message: WireMessage) => {
    const { id } = message

    try {
      const value = await runOperation(transformer, message, context(message))

      // `check` only passes or throws; what it returns means nothing.
      send({ kind: 'result', id, ok: true, value: message.op === 'check' ? null : (value ?? null) })
    } catch (error) {
      if (error instanceof Miss) {
        send({ kind: 'result', id, ok: false, miss: error.message })
        return
      }

      const { message: text, stack } = error instanceof Error ? error : new Error(String(error))
      send({ kind: 'result', id, ok: false, error: text, stack })
    }
  }

  const onMessage = (message: WireMessage) => {
    switch (message.kind) {
      case 'hello':
        send({
          kind: 'ready',
          protocol: PROTOCOL,
          name: transformer.name,
          version: transformer.version,
          directions: [
            ...(transformer.input ? ['input'] : []),
            ...(transformer.output ? ['output'] : []),
          ],
        })
        return

      case 'call':
        // Not awaited: calls of different connections may run at the same time.
        void call(message)
        return

      case 'hostResult':
        settle(message)
        return

      case 'close':
        states.delete(message.connection as string)
        return

      case 'shutdown':
        io.exit(0)
        return
    }
  }

  read(io.input, onMessage, (error) => {
    console.error(`decoy Transformer worker ${transformer.name}: ${error.message}`)
    io.exit(1)
  })

  // Decoy went away without `shutdown`: don't outlive it.
  io.input.on('end', () => {
    io.exit(0)
  })
}
