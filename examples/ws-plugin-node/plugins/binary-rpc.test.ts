// A plugin is a plain object outside Decoy, so its steps are unit-testable without a process:
//   node --test plugins/
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { type Context, Miss } from '@decoyhq/plugin'
import { decodeProto, encodeProto, protos } from '../tests/decoy.ts'
import plugin from './binary-rpc.ts'

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

const frame = plugin.steps.frame

void test('a REQUEST round-trips through encode and decode', async () => {
  const message = { kind: 'REQUEST', topic: 'users.get', id: 7, payload: { id: 'u-1' } }
  const bytes = (await frame.encode(message, ctx())) as Uint8Array
  assert.equal(bytes[0], 1) // the REQUEST byte

  assert.deepEqual(await frame.decode(bytes, ctx('binary')), message)
})

void test('an ERROR carries a code and a reason instead of a payload', async () => {
  const message = { kind: 'ERROR', topic: 'users.get', id: 7, error: { code: 404, reason: 'gone' } }
  const bytes = (await frame.encode(message, ctx())) as Uint8Array

  assert.deepEqual(await frame.decode(bytes, ctx('binary')), message)
})

void test('events without an id are numbered per connection', async () => {
  const state = new Map<string, unknown>()
  const event = { kind: 'EVENT', topic: 'users.updated', payload: { id: 'u' } }

  await frame.encode(event, ctx(undefined, state))
  await frame.encode(event, ctx(undefined, state))

  assert.equal(state.get('events'), 2)
})

void test('an unknown kind byte is a miss', async () => {
  await assert.rejects(Promise.resolve(frame.decode(Uint8Array.from([9, 0]), ctx('binary'))), Miss)
})
