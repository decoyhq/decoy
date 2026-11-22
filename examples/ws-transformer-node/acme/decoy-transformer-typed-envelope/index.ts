// @acme/decoy-transformer-typed-envelope, as its author would publish it (a stand-in third-party
// package). Binary frames are msgpack maps that name their own proto type:
//
//   { <typeField>: "approvals.v1.Approval", <dataField>: <protobuf bytes> }
//
// Text frames are plain JSON. Options are typed, with defaults.
import { decode, encode } from '@msgpack/msgpack'
import { defineTransformer, miss } from '@decoyhq/transformer'

export interface TypedEnvelopeOptions {
  typeField?: string
  dataField?: string
}

type Envelope = Record<string, unknown>

export function typedEnvelope({
  typeField = 'type',
  dataField = 'data',
}: TypedEnvelopeOptions = {}) {
  const isEnvelope = (value: unknown): value is Envelope =>
    typeof value === 'object' && value !== null && typeField in value

  return defineTransformer({
    name: 'typed-envelope',
    version: '1.1.0',

    // Inbound: frame bytes → the JSON the Cases match on.
    async input(bytes, ctx) {
      if (!(bytes instanceof Uint8Array)) return bytes

      if (ctx.frame === 'text') {
        const text = new TextDecoder().decode(bytes)
        try {
          return JSON.parse(text) as unknown
        } catch {
          return text
        }
      }

      // `miss` skips the frame: Decoy logs the reason and keeps the socket open.
      let envelope: unknown
      try {
        envelope = decode(bytes)
      } catch {
        miss('binary frame is not msgpack')
      }

      if (!isEnvelope(envelope)) miss('binary frame is missing its envelope type')
      const type = envelope[typeField]
      if (typeof type !== 'string' || !(await ctx.protobuf.has(type))) {
        miss(`unknown type ${String(type)}`)
      }

      // Decoy's compiled protos; the transformer loads no .proto files.
      const data = await ctx.protobuf.decode(type, envelope[dataField] as Uint8Array)

      return { ...envelope, [dataField]: data }
    },

    // Outbound: a rendered message → a string (text frame) or bytes (binary frame).
    async output(value, ctx) {
      if (!isEnvelope(value)) return JSON.stringify(value)

      const type = value[typeField] as string
      const data = await ctx.protobuf.encode(type, value[dataField])

      return encode({ ...value, [dataField]: data })
    },

    // At startup, on each outbound literal as written: a typo in `data` fails here.
    async check(value, ctx) {
      if (!isEnvelope(value)) return

      await ctx.protobuf.encode(value[typeField] as string, value[dataField])
    },
  })
}
