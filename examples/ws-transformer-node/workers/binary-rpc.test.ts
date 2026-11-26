// A transformer is a plain object outside Decoy, so its steps are unit-testable without a process:
//   node --test workers/
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { binaryRpc } from '@acme/decoy-transformer-binary-rpc'
import { Miss } from '@decoyhq/transformer'
import type { Context } from '@decoyhq/transformer'
import { decodeProto, encodeProto, protos } from '../tests/decoy.ts'
import transformer from './binary-rpc.ts'
import envelope from './typed-envelope.ts'

/** What Decoy hands a step; protobufjs stands in for Decoy's protos. */
const ctx = (frame?: 'text' | 'binary', state = new Map<string, unknown>()): Context => ({
  frame,
  connection: { id: 'c-1', route: 'rpc', session: 'test' },
  state,
  protobuf: {
    has: (type) => Promise.resolve(protos.lookup(type) !== null),
    decode: (type, bytes) => Promise.resolve(decodeProto(type, bytes) as never),
    encode: (type, value) => Promise.resolve(encodeProto(type, value as object)),
  },
})

void test('a REQUEST round-trips through output and input', async () => {
  const message = { kind: 'REQUEST', topic: 'users.get', id: 7, payload: { id: 'u-1' } }
  const bytes = (await transformer.output(message, ctx())) as Uint8Array
  assert.equal(bytes[0], 1) // the REQUEST byte

  assert.deepEqual(await transformer.input(bytes, ctx('binary')), message)
})

void test('an ERROR carries a code and a reason instead of a payload', async () => {
  const message = { kind: 'ERROR', topic: 'users.get', id: 7, error: { code: 404, reason: 'gone' } }
  const bytes = (await transformer.output(message, ctx())) as Uint8Array

  assert.deepEqual(await transformer.input(bytes, ctx('binary')), message)
})

void test('events without an id are numbered per connection', async () => {
  const state = new Map<string, unknown>()
  const event = { kind: 'EVENT', topic: 'users.updated', payload: { id: 'u' } }

  await transformer.output(event, ctx(undefined, state))
  await transformer.output(event, ctx(undefined, state))

  assert.equal(state.get('events'), 2)
})

void test('an unknown kind byte is a miss', async () => {
  await assert.rejects(
    Promise.resolve(transformer.input(Uint8Array.from([9, 0]), ctx('binary'))),
    Miss,
  )
})

void test('kind overrides must be distinct bytes', () => {
  const withCode = (code: number) => binaryRpc({ topics: {}, kinds: { REQUEST: code } })
  for (const code of [-1, 1.5, 257, Number.NaN]) {
    assert.throws(() => withCode(code), /integers from 0 through 255/)
  }
  assert.throws(() => withCode(2), /own byte/)
  assert.doesNotThrow(() => withCode(0))
  assert.doesNotThrow(() => withCode(255))
})

void test('malformed frames are misses and oversized RPC fields are rejected', async () => {
  for (const body of [[], [0]]) {
    const bytes = Uint8Array.from([3, 0, 0, 0, 0, 0, 0, ...body])
    await assert.rejects(Promise.resolve(transformer.input(bytes, ctx('binary'))), Miss)
  }
  await assert.rejects(Promise.resolve(envelope.input(Uint8Array.of(0xc0), ctx('binary'))), Miss)
  const error = { kind: 'ERROR', topic: 'users.get', id: 1 }
  for (const bad of [{ id: 2 ** 32 }, { error: { code: 65_537 } }, { topic: 'x'.repeat(65_536) }]) {
    await assert.rejects(
      Promise.resolve(transformer.output({ ...error, ...bad }, ctx())),
      /must be an integer/,
    )
  }
})
