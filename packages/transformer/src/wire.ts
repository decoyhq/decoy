import { decode, encode } from '@msgpack/msgpack'
import type { Readable, Writable } from 'node:stream'

/** One protocol message: a msgpack map with a string `kind` (`apps/docs/content/en/docs/transformer-worker-protocol/index.md`). */
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
  // Spare capacity follows the unread bytes. Never reuse consumed storage: decoded
  // binary values may still reference it.
  let buffer: Buffer | undefined
  let used = 0

  input.on('data', (chunk: Buffer) => {
    if (!buffer) {
      buffer = Buffer.from(chunk)
    } else {
      const needed = used + chunk.length
      if (needed > buffer.length) {
        const capacity = Math.max(needed, Math.min(used * 2, MAX_MESSAGE + 4))
        const grown = Buffer.allocUnsafe(capacity)
        buffer.copy(grown, 0, 0, used)
        buffer = grown
      }
      chunk.copy(buffer, used)
    }
    used += chunk.length
    const beforeDrain = buffer

    while (used >= 4) {
      const length = buffer.readUInt32BE(0)
      if (length > MAX_MESSAGE) {
        onError(new Error(`message of ${length} bytes is over the ${MAX_MESSAGE} byte limit`))
        return
      }

      // Wait for the rest of this message.
      const end = 4 + length
      if (used < end) break

      const body = buffer.subarray(4, end)
      buffer = buffer.subarray(end)
      used -= end
      if (used === 0) buffer = undefined

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
      if (!buffer) return
    }

    // Drain complete frames before copying any tail. Don't pin a large backing
    // allocation for a small partial frame, or compact ordinary pooled buffers.
    const backingSize = buffer.buffer.byteLength
    if (buffer !== beforeDrain && backingSize > Buffer.poolSize && used * 2 <= backingSize) {
      buffer = Buffer.from(buffer.subarray(0, used))
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
