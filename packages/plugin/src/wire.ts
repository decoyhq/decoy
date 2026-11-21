import { decode, encode } from '@msgpack/msgpack'
import type { Readable, Writable } from 'node:stream'

/** One protocol message: a msgpack map with a string `kind` (`docs/reference/plugin-protocol.md`). */
export type WireMessage = Record<string, unknown> & { kind: string }

/** Longest message either side may send. */
export const MAX_MESSAGE = 16 * 1024 * 1024

/** Writes `message` as a 4-byte big-endian length, then its msgpack bytes. */
export function write(out: Writable, message: WireMessage): void {
  const body = encode(message)
  if (body.length > MAX_MESSAGE) {
    throw new Error(`message of ${body.length} bytes is over the ${MAX_MESSAGE} byte limit`)
  }

  const head = Buffer.alloc(4)
  head.writeUInt32BE(body.length)

  out.write(Buffer.concat([head, body]))
}

/** Calls `onMessage` for each complete message on `input`; a malformed one calls `onError`. */
export function read(
  input: Readable,
  onMessage: (message: WireMessage) => void,
  onError: (error: Error) => void,
): void {
  // Bytes received but not yet a whole message.
  let buffer = Buffer.alloc(0)

  input.on('data', (chunk: Buffer) => {
    buffer = Buffer.concat([buffer, chunk])

    while (buffer.length >= 4) {
      const length = buffer.readUInt32BE(0)
      if (length > MAX_MESSAGE) {
        onError(new Error(`message of ${length} bytes is over the ${MAX_MESSAGE} byte limit`))
        return
      }

      // Wait for the rest of this message.
      if (buffer.length < 4 + length) return

      const body = buffer.subarray(4, 4 + length)
      buffer = buffer.subarray(4 + length)

      let message: unknown
      try {
        message = decodeMessage(body)
      } catch (error) {
        onError(error instanceof Error ? error : new Error(String(error)))
        return
      }
      if (!isWireMessage(message)) {
        onError(new Error('message is not a map with a `kind`'))
        return
      }

      onMessage(message)
    }
  })
}

/** Preserves valid JSON keys without changing object prototypes. */
function decodeMessage(body: Uint8Array): unknown {
  try {
    return decode(body)
  } catch (error) {
    if (!(error instanceof Error) || error.message !== 'The key __proto__ is not allowed') {
      throw error
    }
  }

  const utf8 = new TextDecoder('utf-8', { ignoreBOM: true })
  const value = decode(body, {
    keyDecoder: {
      canBeCached: () => true,
      decode: (bytes, offset, length) => '_' + utf8.decode(bytes.subarray(offset, offset + length)),
    },
  })
  return restoreKeys(value)
}

/** Removes the key prefix and restores own properties, including `__proto__`. */
function restoreKeys(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(restoreKeys)
  if (!value || typeof value !== 'object' || Object.getPrototypeOf(value) !== Object.prototype) {
    return value
  }

  return Object.fromEntries(
    Object.entries(value).map(([key, entry]) => [
      key.startsWith('_') ? key.slice(1) : key,
      restoreKeys(entry),
    ]),
  )
}

/** Checks that the protocol message has a string `kind` field. */
function isWireMessage(value: unknown): value is WireMessage {
  return (
    typeof value === 'object' && value !== null && 'kind' in value && typeof value.kind === 'string'
  )
}
