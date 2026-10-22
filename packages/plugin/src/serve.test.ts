import { PassThrough } from 'node:stream'
import { describe, expect, test } from '@rstest/core'
import { definePlugin, miss, type Plugin } from './index'
import { serve } from './serve'
import { read, type WireMessage, write } from './wire'

/** Serves `plugin` on in-memory pipes; `next()` resolves with its next message to Decoy. */
function start(plugin: Plugin) {
  const toPlugin = new PassThrough()
  const fromPlugin = new PassThrough()
  const exits: number[] = []
  serve(plugin, { input: toPlugin, output: fromPlugin, exit: (code) => exits.push(code) })

  // What the plugin wrote back, for `next()` to hand out in order.
  const inbox: WireMessage[] = []
  const waiting: Array<(message: WireMessage) => void> = []
  read(
    fromPlugin,
    (message) => {
      const resolve = waiting.shift()
      if (resolve) resolve(message)
      else inbox.push(message)
    },
    (error) => {
      throw error
    },
  )

  return {
    exits,
    input: toPlugin,
    send: (message: WireMessage) => {
      write(toPlugin, message)
    },
    next: () =>
      new Promise<WireMessage>((resolve) => {
        const message = inbox.shift()
        if (message) resolve(message)
        else waiting.push(resolve)
      }),
  }
}

const connection = { id: 'c-1', route: 'live', session: 's-1' }

const envelope = definePlugin({
  name: 'envelope',
  version: '1.2.3',
  steps: {
    envelope: {
      async decode(bytes, ctx) {
        const text = new TextDecoder().decode(bytes)

        // A few frames that drive the edge cases.
        if (text === 'nope') miss('not an envelope')
        if (text === 'boom') throw new Error('bad frame')
        if (text === 'proto') return ctx.protobuf.decode('users.v1.User', bytes)

        // Counts frames per connection, to show `ctx.state`.
        const seen = ((ctx.state.get('seen') as number | undefined) ?? 0) + 1
        ctx.state.set('seen', seen)

        return { text, frame: ctx.frame, seen, route: ctx.connection?.route }
      },
      encode(value) {
        return typeof value === 'string' ? value : new TextEncoder().encode(JSON.stringify(value))
      },
    },
  },
})

describe('serve', () => {
  test('answers hello with the protocol, its name and its steps', async () => {
    const plugin = start(envelope)
    plugin.send({ kind: 'hello', protocol: 1, decoy: '0.4.0' })
    expect(await plugin.next()).toEqual({
      kind: 'ready',
      protocol: 1,
      name: 'envelope',
      version: '1.2.3',
      steps: ['envelope'],
      functions: [],
    })
  })

  test('runs calls with their frame, connection and per-connection state', async () => {
    const plugin = start(envelope)
    const decode = (id: number, text: string, conn = connection) => {
      plugin.send({
        kind: 'call',
        id,
        op: 'decode',
        step: 'envelope',
        input: new TextEncoder().encode(text),
        frame: 'text',
        connection: conn,
      })
    }
    decode(1, 'hi')
    expect(await plugin.next()).toEqual({
      kind: 'result',
      id: 1,
      ok: true,
      value: { text: 'hi', frame: 'text', seen: 1, route: 'live' },
    })
    decode(2, 'again')
    expect(await plugin.next()).toMatchObject({ id: 2, value: { seen: 2 } })
    decode(3, 'other', { ...connection, id: 'c-2' })
    expect(await plugin.next()).toMatchObject({ id: 3, value: { seen: 1 } })

    plugin.send({ kind: 'close', connection: 'c-1' })
    decode(4, 'reopened')
    expect(await plugin.next()).toMatchObject({ id: 4, value: { seen: 1 } })

    plugin.send({ kind: 'call', id: 5, op: 'encode', step: 'envelope', input: { a: 1 } })
    const encoded = await plugin.next()
    expect(encoded).toMatchObject({ kind: 'result', id: 5, ok: true })
    expect(new TextDecoder().decode(encoded.value as Uint8Array)).toBe('{"a":1}')
  })

  test('reports a miss, a thrown error with its stack, and an unknown step', async () => {
    const plugin = start(envelope)
    const call = (id: number, step: string, text: string) => {
      plugin.send({
        kind: 'call',
        id,
        op: 'decode',
        step,
        input: new TextEncoder().encode(text),
        frame: 'binary',
      })
    }
    call(1, 'envelope', 'nope')
    expect(await plugin.next()).toEqual({
      kind: 'result',
      id: 1,
      ok: false,
      miss: 'not an envelope',
    })
    call(2, 'envelope', 'boom')
    const failed = await plugin.next()
    expect(failed).toMatchObject({ id: 2, ok: false, error: 'bad frame' })
    expect(failed.stack).toContain('bad frame')
    call(3, 'missing', 'x')
    expect(await plugin.next()).toMatchObject({
      id: 3,
      ok: false,
      error: 'plugin envelope has no step `missing`',
    })
  })

  test('a step without check passes it', async () => {
    const plugin = start(envelope)
    plugin.send({ kind: 'call', id: 1, op: 'check', step: 'envelope', input: { a: '${ x }' } })
    expect(await plugin.next()).toEqual({ kind: 'result', id: 1, ok: true, value: null })
  })

  test('asks Decoy for protobuf work with host calls', async () => {
    const plugin = start(envelope)
    const bytes = new TextEncoder().encode('proto')
    plugin.send({
      kind: 'call',
      id: 1,
      op: 'decode',
      step: 'envelope',
      input: bytes,
      frame: 'binary',
    })
    const ask = await plugin.next()
    expect(ask).toMatchObject({ kind: 'host', id: 1, op: 'protobuf.decode' })
    const [type, sent] = ask.args as [string, Uint8Array]
    expect([type, new TextDecoder().decode(sent)]).toEqual(['users.v1.User', 'proto'])
    plugin.send({ kind: 'hostResult', id: ask.id, ok: true, value: { name: 'Ada' } })
    expect(await plugin.next()).toEqual({ kind: 'result', id: 1, ok: true, value: { name: 'Ada' } })

    plugin.send({
      kind: 'call',
      id: 2,
      op: 'decode',
      step: 'envelope',
      input: bytes,
      frame: 'binary',
    })
    const again = await plugin.next()
    plugin.send({ kind: 'hostResult', id: again.id, ok: false, error: 'users.v1.User not found' })
    expect(await plugin.next()).toMatchObject({
      id: 2,
      ok: false,
      error: 'users.v1.User not found',
    })
  })

  test('exits on shutdown, and when Decoy closes its stdin', () => {
    const stopped = start(envelope)
    stopped.send({ kind: 'shutdown' })
    expect(stopped.exits).toEqual([0])

    const orphaned = start(envelope)
    orphaned.input.end()
    orphaned.input.resume()
    return new Promise<void>((done) => {
      setImmediate(() => {
        expect(orphaned.exits).toEqual([0])
        done()
      })
    })
  })
})

describe('definePlugin', () => {
  test('returns the plugin without serving it outside Decoy', () => {
    expect(process.env.DECOY_PLUGIN).toBeUndefined()
    expect(envelope.name).toBe('envelope')
    expect(Object.keys(envelope.steps)).toEqual(['envelope'])
  })
})
