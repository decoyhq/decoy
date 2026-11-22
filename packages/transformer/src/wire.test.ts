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

test('a partial frame does not retain storage from a preceding large frame', () => {
  const serialized = new PassThrough()
  const first = { kind: 'call', text: 'x'.repeat(128 * 1024) }
  const second = { kind: 'call', binary: Buffer.alloc(16, 42) }
  write(serialized, first)
  const firstFrame: unknown = serialized.read()
  ok(Buffer.isBuffer(firstFrame))
  write(serialized, second)
  const secondFrame: unknown = serialized.read()
  ok(Buffer.isBuffer(secondFrame))

  const input = new PassThrough()
  const messages: WireMessage[] = []
  read(
    input,
    (message) => messages.push(message),
    (error) => {
      throw error
    },
  )
  const split = firstFrame.length - 32
  input.write(firstFrame.subarray(0, split))
  input.write(Buffer.concat([firstFrame.subarray(split), secondFrame.subarray(0, 8)]))
  expect(messages).toEqual([first])
  input.write(secondFrame.subarray(8))

  const binary = messages[1]?.binary
  expect(binary).toEqual(second.binary)
  ok(Buffer.isBuffer(binary))
  expect(binary.buffer.byteLength).toBeLessThan(firstFrame.length)
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
