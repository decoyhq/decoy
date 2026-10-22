import type { Readable, Writable } from 'node:stream'
import { type Connection, type Context, type Json, Miss, PROTOCOL, type Plugin } from './index'
import { read, type WireMessage, write } from './wire'

interface Io {
  input: Readable
  output: Writable

  /** Called once Decoy asks the plugin to stop, or stops talking to it. */
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

/** Answers Decoy's messages on `io` for `plugin` (`docs/design/plugin-protocol.md`). */
export function serve(plugin: Plugin, io: Io = processIo): void {
  const send = (message: WireMessage) => {
    write(io.output, message)
  }

  // `ctx.state` of each open connection, by connection id.
  const states = new Map<string, Map<string, unknown>>()

  // Host calls waiting for Decoy's answer, by the id the plugin gave them.
  const hostCalls = new Map<number, HostCall>()
  let nextHostId = 1

  const host = (op: string, args: unknown[]) =>
    new Promise<unknown>((resolve, reject) => {
      const id = nextHostId++
      hostCalls.set(id, { resolve, reject })
      send({ kind: 'host', id, op, args })
    })

  // Decoy's compiled protos, reached through host calls.
  const protobuf: Context['protobuf'] = {
    has: async (type) => (await host('protobuf.has', [type])) as boolean,
    decode: async (type, bytes) => (await host('protobuf.decode', [type, bytes])) as Json,
    encode: async (type, value) => (await host('protobuf.encode', [type, value])) as Uint8Array,
  }

  const context = (message: WireMessage): Context => {
    const connection = message.connection as Connection | undefined

    // A call without a connection (startup `check`) gets a fresh state that nobody keeps.
    let state = connection && states.get(connection.id)
    if (!state) {
      state = new Map()
      if (connection) states.set(connection.id, state)
    }

    const frame = message.frame as Context['frame']
    return { frame, connection, state, protobuf }
  }

  const run = (step: Plugin['steps'][string], message: WireMessage, ctx: Context) => {
    const input = message.input as Uint8Array

    switch (message.op) {
      case 'decode':
        return step.decode(input, ctx)
      case 'encode':
        return step.encode(input, ctx)
      default:
        return step.check?.(input, ctx)
    }
  }

  const call = async (message: WireMessage) => {
    const { id } = message

    try {
      const step = plugin.steps[message.step as string]
      if (!step) throw new Error(`plugin ${plugin.name} has no step \`${String(message.step)}\``)

      const value = await run(step, message, context(message))

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
          name: plugin.name,
          version: plugin.version,
          steps: Object.keys(plugin.steps),
          functions: [],
        })
        return

      case 'call':
        // Not awaited: calls of different connections may run at the same time.
        void call(message)
        return

      case 'hostResult': {
        const pending = hostCalls.get(message.id as number)
        hostCalls.delete(message.id as number)

        if (message.ok) pending?.resolve(message.value)
        else pending?.reject(new Error(String(message.error)))
        return
      }

      case 'close':
        states.delete(message.connection as string)
        return

      case 'shutdown':
        io.exit(0)
        return
    }
  }

  read(io.input, onMessage, (error) => {
    console.error(`decoy plugin ${plugin.name}: ${error.message}`)
    io.exit(1)
  })

  // Decoy went away without `shutdown`: don't outlive it.
  io.input.on('end', () => {
    io.exit(0)
  })
}
