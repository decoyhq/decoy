import { definePlugin } from '@decoyhq/plugin'
import { createCodec } from './codec.mjs'

const [schemaMap, topic] = process.argv.slice(2)
const codec = createCodec(schemaMap)

definePlugin({
  name: 'transformer-prototype',
  version: '0.0.0',
  steps: {
    sdk: { decode: codec.inbound, encode: (message) => codec.outbound(message, topic) },
    'sdk-host': {
      decode: (bytes, ctx) => codec.inboundHost(bytes, ctx.protobuf),
      encode: (message, ctx) => codec.outboundHost(message, topic, ctx.protobuf),
    },
  },
})
