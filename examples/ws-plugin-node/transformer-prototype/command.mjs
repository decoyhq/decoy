import { readFileSync } from 'node:fs'
import { encode } from '@msgpack/msgpack'
import { createCodec } from './codec.mjs'

const [operation, schemaMap, topic] = process.argv.slice(2)
const codec = createCodec(schemaMap)
const input = readFileSync(0)

switch (operation) {
  case 'inbound':
    process.stdout.write(JSON.stringify(codec.inbound(input)))
    break
  case 'outbound':
    process.stdout.write(codec.outbound(JSON.parse(input), topic))
    break
  case 'strip-grpc':
    process.stdout.write(codec.stripGrpc(input))
    break
  case 'read-proto':
    process.stdout.write(JSON.stringify(codec.readProto(input)))
    break
  case 'write-proto':
    process.stdout.write(encode(codec.writeProto(JSON.parse(input), topic)))
    break
  case 'add-grpc':
    process.stdout.write(codec.addGrpc(input))
    break
  default:
    throw new Error(`Unknown operation: ${operation}`)
}
