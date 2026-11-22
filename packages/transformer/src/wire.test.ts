import { ok } from 'node:assert/strict'
import { PassThrough } from 'node:stream'
import { expect, test } from '@rstest/core'
import { read, type WireMessage, write } from './wire'

test('waits for a split header and payload, then reads all complete messages in order', () => {
  const first = { kind: 'call', id: 1 }
  const second = { kind: 'shutdown' }
  const serialized = new PassThrough()
  write(serialized, first)
  write(serialized, second)
  const bytes: unknown = serialized.read()
  ok(Buffer.isBuffer(bytes))

  const input = new PassThrough()
  const messages: WireMessage[] = []
  const errors: Error[] = []
  read(
    input,
    (message) => messages.push(message),
    (error) => errors.push(error),
  )

  input.write(bytes.subarray(0, 2))
  expect(messages).toEqual([])
  input.write(bytes.subarray(2, 5))
  expect(messages).toEqual([])
  input.write(bytes.subarray(5))
  expect(errors).toEqual([])
  expect(messages).toEqual([first, second])
})

test('preserves binary values after receiving and consuming more chunks', () => {
  const serialized = new PassThrough()
  const first = { kind: 'call', binary: Buffer.from([1, 2, 3]) }
  const second = { kind: 'call', binary: Buffer.alloc(32 * 1024, 42) }
  write(serialized, first)
  write(serialized, second)
  const bytes: unknown = serialized.read()
  ok(Buffer.isBuffer(bytes))

  const input = new PassThrough()
  const messages: WireMessage[] = []
  read(
    input,
    (message) => messages.push(message),
    (error) => {
      throw error
    },
  )
  for (let start = 0; start < bytes.length; start += 1024) {
    input.write(bytes.subarray(start, start + 1024))
  }
  bytes.fill(0)
  expect(messages).toEqual([first, second])
})

test('rejects a valid msgpack value without a protocol kind', () => {
  const input = new PassThrough()
  const messages: WireMessage[] = []
  const errors: string[] = []
  read(
    input,
    (message) => messages.push(message),
    (error) => errors.push(error.message),
  )
  // A one-byte body containing msgpack nil.
  input.write(Buffer.from([0, 0, 0, 1, 0xc0]))
  expect(messages).toEqual([])
  expect(errors).toEqual(['message is not a map with a `kind`'])
})
