// @acme/decoy-plugin-binary-rpc, as its author would publish it (a stand-in third-party package).
// One binary frame is one RPC message:
//
//   [u8 kind][u16 topic length][topic, UTF-8][u32 id][body]
//
// The body is a protobuf message for REQUEST, REPLY and EVENT (its type set per topic), and
// [u16 code][reason, UTF-8] for ERROR. Text frames are plain JSON. Cases see
// { kind, topic, id, payload } or, for ERROR, { kind, topic, id, error: { code, reason } }.
import { definePlugin, miss } from '@decoyhq/plugin'

export type Kind = 'REQUEST' | 'REPLY' | 'ERROR' | 'EVENT'

export interface BinaryRpcOptions {
  /** Proto message types by topic: what a REQUEST carries, what its REPLY and EVENTs carry. */
  topics: Record<string, { request?: string; reply?: string; event?: string }>

  /** The byte each kind is sent as; defaults to REQUEST 1, REPLY 2, ERROR 3, EVENT 4. */
  kinds?: Partial<Record<Kind, number>>
}

interface Message {
  kind: Kind
  topic: string
  id?: number
  payload?: unknown
  error?: { code: number; reason: string }
}

const KINDS: Kind[] = ['REQUEST', 'REPLY', 'ERROR', 'EVENT']

const text = new TextEncoder()
const utf8 = new TextDecoder()

export function binaryRpc({ topics, kinds = {} }: BinaryRpcOptions) {
  const codes: Record<Kind, number> = { REQUEST: 1, REPLY: 2, ERROR: 3, EVENT: 4, ...kinds }

  // Bad options throw here, before `ready`: Decoy fails startup and shows this message.
  const values = Object.values(codes)
  if (values.some((code) => !Number.isInteger(code) || code < 0 || code > 255)) {
    throw new Error('binaryRpc: kind codes must be integers from 0 through 255')
  }
  if (new Set(values).size !== KINDS.length) {
    throw new Error('binaryRpc: every kind needs its own byte')
  }

  const kindOf = (code: number | undefined) => KINDS.find((kind) => codes[kind] === code)

  /** The proto type of a message's payload, from the topic's options. */
  const typeOf = (m: Message) => {
    const types = topics[m.topic]
    if (m.kind === 'REQUEST') return types?.request
    if (m.kind === 'EVENT') return types?.event
    return types?.reply
  }

  const isMessage = (value: unknown): value is Message =>
    typeof value === 'object' && value !== null && KINDS.includes((value as Message).kind)

  return definePlugin({
    name: 'binary-rpc',
    version: '0.1.0',

    // Inbound: frame bytes → the JSON the Cases match on.
    async input(bytes, ctx) {
      if (!(bytes instanceof Uint8Array)) return bytes

      if (ctx.frame === 'text') return JSON.parse(utf8.decode(bytes)) as unknown

      const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)

      // `miss` skips the frame: Decoy logs the reason and keeps the socket open.
      const kind = kindOf(bytes[0])
      if (!kind) miss(`unknown kind byte ${bytes[0]}`)
      if (bytes.length < 3) miss('frame ends before its topic length')

      const topicEnd = 3 + view.getUint16(1)
      if (bytes.length < topicEnd + 4) miss('frame ends before its id')

      const topic = utf8.decode(bytes.subarray(3, topicEnd))
      const id = view.getUint32(topicEnd)
      const body = bytes.subarray(topicEnd + 4)

      if (kind === 'ERROR') {
        const code = body.length >= 2 ? new DataView(body.buffer, body.byteOffset).getUint16(0) : 0
        return { kind, topic, id, error: { code, reason: utf8.decode(body.subarray(2)) } }
      }

      const type = typeOf({ kind, topic })
      if (!type) miss(`no proto type for a ${kind} on topic ${topic}`)

      // Decoy's compiled protos; the plugin loads no .proto files.
      return { kind, topic, id, payload: await ctx.protobuf.decode(type, body) }
    },

    // Outbound: a rendered message → a string (text frame) or bytes (binary frame).
    async output(value, ctx) {
      if (!isMessage(value)) return JSON.stringify(value)

      let id = value.id
      if (id === undefined && value.kind === 'EVENT') {
        // Per-connection state: each socket numbers its events 1, 2, …
        id = ((ctx.state.get('events') as number | undefined) ?? 0) + 1
        ctx.state.set('events', id)
      }

      let body: Uint8Array
      if (value.kind === 'ERROR') {
        const reason = text.encode(value.error?.reason ?? '')
        body = new Uint8Array(2 + reason.length)
        new DataView(body.buffer).setUint16(0, value.error?.code ?? 0)
        body.set(reason, 2)
      } else {
        const type = typeOf(value)
        if (!type) throw new Error(`no proto type for a ${value.kind} on topic ${value.topic}`)
        body = await ctx.protobuf.encode(type, value.payload ?? {})
      }

      const topic = text.encode(value.topic)
      const frame = new Uint8Array(3 + topic.length + 4 + body.length)
      const view = new DataView(frame.buffer)

      frame[0] = codes[value.kind]
      view.setUint16(1, topic.length)
      frame.set(topic, 3)
      view.setUint32(3 + topic.length, id ?? 0)
      frame.set(body, 3 + topic.length + 4)

      return frame
    },

    // At startup, on each outbound literal as written: a typo in a payload fails here.
    async check(value, ctx) {
      if (!isMessage(value) || value.kind === 'ERROR') return

      const type = typeOf(value)
      if (!type) throw new Error(`no proto type for a ${value.kind} on topic ${value.topic}`)

      await ctx.protobuf.encode(type, value.payload ?? {})
    },
  })
}
