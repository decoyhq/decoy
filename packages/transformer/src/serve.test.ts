import { ok } from 'node:assert/strict'
import { PassThrough } from 'node:stream'
import { describe, expect, test } from '@rstest/core'
import { defineTransformer, miss, type Transformer } from './index'
import { serve } from './serve'
import { MAX_MESSAGE, read, type WireMessage, write } from './wire'

/** Serves `transformer` on in-memory pipes; `next()` resolves with its next message to Decoy. */
function start(transformer: Transformer) {
  const toWorker = new PassThrough()
  const fromWorker = new PassThrough()
  const exits: number[] = []
  serve(transformer, { input: toWorker, output: fromWorker, exit: (code) => exits.push(code) })

  // What the transformer wrote back, for `next()` to hand out in order.
  const inbox: WireMessage[] = []
  const waiting: Array<(message: WireMessage) => void> = []
  read(
    fromWorker,
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
    input: toWorker,
    send: (message: WireMessage) => {
      write(toWorker, message)
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

const envelope = defineTransformer({
  name: 'envelope',
  version: '1.2.3',
  async input(bytes, ctx) {
    if (!(bytes instanceof Uint8Array)) return bytes

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
  output(value) {
    return typeof value === 'string' ? value : new TextEncoder().encode(JSON.stringify(value))
  },
})

describe('serve', () => {
  test('answers hello with the protocol, its name and its directions', async () => {
    const transformer = start(envelope)
    transformer.send({ kind: 'hello', protocol: 1, decoy: '0.4.0' })
    expect(await transformer.next()).toEqual({
      kind: 'ready',
      protocol: 1,
      name: 'envelope',
      version: '1.2.3',
      directions: ['input', 'output'],
    })
  })

  test('runs calls with their frame, connection and per-connection state', async () => {
    const transformer = start(envelope)
    const input = (id: number, text: string, conn = connection) => {
      transformer.send({
        kind: 'call',
        id,
        op: 'input',
        input: new TextEncoder().encode(text),
        frame: 'text',
        connection: conn,
      })
    }
    input(1, 'hi')
    expect(await transformer.next()).toEqual({
      kind: 'result',
      id: 1,
      ok: true,
      value: { text: 'hi', frame: 'text', seen: 1, route: 'live' },
    })
    input(2, 'again')
    expect(await transformer.next()).toMatchObject({ id: 2, value: { seen: 2 } })
    input(3, 'other', { ...connection, id: 'c-2' })
    expect(await transformer.next()).toMatchObject({ id: 3, value: { seen: 1 } })

    transformer.send({ kind: 'close', connection: 'c-1' })
    input(4, 'reopened')
    expect(await transformer.next()).toMatchObject({ id: 4, value: { seen: 1 } })

    transformer.send({ kind: 'call', id: 5, op: 'output', input: { a: 1 } })
    const encoded = await transformer.next()
    expect(encoded).toMatchObject({ kind: 'result', id: 5, ok: true })
    ok(encoded.value instanceof Uint8Array)
    expect(new TextDecoder().decode(encoded.value)).toBe('{"a":1}')

    transformer.send({ kind: 'call', id: 6, op: 'input', input: { a: 1 } })
    expect(await transformer.next()).toMatchObject({ id: 6, value: { a: 1 } })
  })

  test.each([false, 0, ''])('preserves the transform result %j', async (value) => {
    const transformer = start({ name: 'identity', version: '1', input: (input) => input })
    transformer.send({ kind: 'call', id: 1, op: 'input', input: value })
    expect(await transformer.next()).toEqual({ kind: 'result', id: 1, ok: true, value })
  })

  test('provides request metadata only on calls that carry it', async () => {
    const transformer = start({
      name: 'request',
      version: '1',
      input: (_value, ctx) => ('request' in ctx ? ctx.request : 'absent'),
    })
    const request = { method: 'POST', path: '/users', headers: { 'content-type': 'text/plain' } }
    transformer.send({ kind: 'call', id: 1, op: 'input', input: 'Ada', request })
    expect(await transformer.next()).toMatchObject({ id: 1, ok: true, value: request })

    transformer.send({ kind: 'call', id: 2, op: 'input', input: 'Ada' })
    expect(await transformer.next()).toMatchObject({ id: 2, ok: true, value: 'absent' })
  })

  test('reports a miss, a thrown error with its stack, and an unavailable direction', async () => {
    const transformer = start({
      name: envelope.name,
      version: envelope.version,
      input: (value, ctx) => envelope.input(value, ctx),
    })
    const call = (id: number, text: string) => {
      transformer.send({
        kind: 'call',
        id,
        op: 'input',
        input: new TextEncoder().encode(text),
        frame: 'binary',
      })
    }
    call(1, 'nope')
    expect(await transformer.next()).toEqual({
      kind: 'result',
      id: 1,
      ok: false,
      miss: 'not an envelope',
    })
    call(2, 'boom')
    const failed = await transformer.next()
    expect(failed).toMatchObject({ id: 2, ok: false, error: 'bad frame' })
    expect(failed.stack).toContain('bad frame')
    transformer.send({ kind: 'call', id: 3, op: 'output', input: 'x' })
    expect(await transformer.next()).toMatchObject({
      id: 3,
      ok: false,
      error: 'Transformer worker envelope does not provide output',
    })
  })

  test('a worker without check passes it', async () => {
    const transformer = start(envelope)
    transformer.send({ kind: 'call', id: 1, op: 'check', input: { a: '${ x }' } })
    expect(await transformer.next()).toEqual({ kind: 'result', id: 1, ok: true, value: null })
  })

  test('asks Decoy for protobuf work with host calls', async () => {
    const transformer = start(envelope)
    const bytes = new TextEncoder().encode('proto')
    transformer.send({
      kind: 'call',
      id: 1,
      op: 'input',
      input: bytes,
      frame: 'binary',
    })
    const ask = await transformer.next()
    expect(ask).toMatchObject({ kind: 'host', id: 1, op: 'protobuf.decode' })
    const [type, sent] = ask.args as [string, Uint8Array]
    expect([type, new TextDecoder().decode(sent)]).toEqual(['users.v1.User', 'proto'])
    transformer.send({ kind: 'hostResult', id: ask.id, ok: true, value: { name: 'Ada' } })
    expect(await transformer.next()).toEqual({
      kind: 'result',
      id: 1,
      ok: true,
      value: { name: 'Ada' },
    })

    // The host call is already settled. Duplicate replies must not interrupt later calls.
    transformer.send({ kind: 'hostResult', id: ask.id, ok: true, value: null })
    transformer.send({ kind: 'hostResult', id: ask.id, ok: false, error: 'late reply' })

    transformer.send({
      kind: 'call',
      id: 2,
      op: 'input',
      input: bytes,
      frame: 'binary',
    })
    const again = await transformer.next()
    transformer.send({
      kind: 'hostResult',
      id: again.id,
      ok: false,
      error: 'users.v1.User not found',
    })
    expect(await transformer.next()).toMatchObject({
      id: 2,
      ok: false,
      error: 'users.v1.User not found',
    })
  })

  test('matches out-of-order host replies to the waiting calls', async () => {
    const transformer = start({
      name: 'lookup',
      version: '1',
      input: (value, ctx) => ctx.protobuf.has(String(value)),
    })
    transformer.send({ kind: 'call', id: 1, op: 'input', input: 'Missing' })
    transformer.send({ kind: 'call', id: 2, op: 'input', input: 'Present' })
    const missing = await transformer.next()
    const present = await transformer.next()
    expect(missing).toMatchObject({ kind: 'host', op: 'protobuf.has', args: ['Missing'] })
    expect(present).toMatchObject({ kind: 'host', op: 'protobuf.has', args: ['Present'] })

    transformer.send({ kind: 'hostResult', id: present.id, ok: true, value: true })
    expect(await transformer.next()).toMatchObject({ kind: 'result', id: 2, ok: true, value: true })
    transformer.send({ kind: 'hostResult', id: missing.id, ok: true, value: false })
    expect(await transformer.next()).toMatchObject({
      kind: 'result',
      id: 1,
      ok: true,
      value: false,
    })
  })

  test('preserves JSON fields named __proto__', async () => {
    const transformer = start(envelope)
    const value: unknown = JSON.parse('{"__proto__":{"field":1},"_keep":2}')
    transformer.send({ kind: 'call', id: 1, op: 'input', input: value })
    expect(await transformer.next()).toEqual({ kind: 'result', id: 1, ok: true, value })
  })

  test('reports a result over the protocol limit and stays ready', async () => {
    const transformer = start({
      name: 'large',
      version: '1',
      output: (value) => (value === 'large' ? new Uint8Array(MAX_MESSAGE) : value),
    })
    transformer.send({ kind: 'call', id: 1, op: 'output', input: 'large' })
    expect(await transformer.next()).toMatchObject({ id: 1, ok: false })
    transformer.send({ kind: 'call', id: 2, op: 'output', input: 'small' })
    expect(await transformer.next()).toEqual({ kind: 'result', id: 2, ok: true, value: 'small' })
  })

  test('exits with an error on an oversized incoming frame', () => {
    const transformer = start(envelope)
    const length = Buffer.alloc(4)
    length.writeUInt32BE(MAX_MESSAGE + 1)
    transformer.input.write(length)
    expect(transformer.exits).toEqual([1])
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

describe('defineTransformer', () => {
  test('returns the transformer without serving it outside Decoy', () => {
    expect(process.env.DECOY_TRANSFORMER).toBeUndefined()
    expect(envelope.name).toBe('envelope')
    expect(typeof envelope.input).toBe('function')
    expect(typeof envelope.output).toBe('function')
  })
})
