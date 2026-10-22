import { decode, encode } from '@msgpack/msgpack'
import type { Readable, Writable } from 'node:stream'

/** One protocol message: a msgpack map with a string `kind` (`docs/design/plugin-protocol.md`). */
export type WireMessage = Record<string, unknown> & { kind: string }

/** Longest message either side may send. */
export const MAX_MESSAGE = 16 * 1024 * 1024

/** Writes `message` as a 4-byte big-endian length, then its msgpack bytes. */
export function write(out: Writable, message: WireMessage): void {
  const body = encode(message)

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

      const message = decode(body)
      if (typeof message !== 'object' || message === null || !('kind' in message)) {
        onError(new Error('message is not a map with a `kind`'))
        return
      }

      onMessage(message as WireMessage)
    }
  })
}
