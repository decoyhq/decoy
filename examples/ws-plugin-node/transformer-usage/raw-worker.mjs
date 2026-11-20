import { decode, encode } from '@msgpack/msgpack'
import { createCodec } from '../transformer-prototype/codec.mjs'

const MAX_MESSAGE = 16 * 1024 * 1024
const PROTOCOL = 1
const [schemaMap, topic] = process.argv.slice(2)
const codec = createCodec(schemaMap)
const pending = new Map()
let nextId = 1
let buffer = Buffer.alloc(0)

/** Пишет одну запись протокола целиком, сохраняя stdout для данных Decoy. */
function send(message) {
  const body = Buffer.from(encode(message))
  if (body.length > MAX_MESSAGE) throw new Error('Protocol message exceeds 16 MiB')
  const prefix = Buffer.alloc(4)
  prefix.writeUInt32BE(body.length)
  process.stdout.write(Buffer.concat([prefix, body]))
}

/** Запрашивает protobuf-операцию у Decoy и ждёт hostResult с тем же id. */
function host(op, args) {
  const id = nextId++
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject })
    send({ kind: 'host', id, op, args })
  })
}

/** Выполняет запрос Decoy, пока цикл чтения принимает ответы на host-вызовы. */
async function call({ id, op, input }) {
  try {
    const proto = {
      decode: (schema, bytes) => host('protobuf.decode', [schema, bytes]),
      encode: (schema, message) => host('protobuf.encode', [schema, message]),
    }
    let value
    switch (op) {
      case 'decode':
        value = await codec.inboundHost(input, proto)
        break
      case 'encode':
        value = await codec.outboundHost(input, topic, proto)
        break
      case 'check':
        await codec.outboundHost(input, topic, proto)
        value = null
        break
      default:
        throw new Error(`Unknown operation: ${op}`)
    }
    send({ kind: 'result', id, ok: true, value })
  } catch (error) {
    send({ kind: 'result', id, ok: false, error: String(error) })
  }
}

/** Обрабатывает сообщения v1 без библиотеки Decoy. */
function receive(message) {
  switch (message.kind) {
    case 'hello':
      if (message.protocol !== PROTOCOL) throw new Error('Unsupported Decoy protocol')
      send({
        kind: 'ready',
        protocol: PROTOCOL,
        name: 'raw-worker',
        version: '0.0.0',
        steps: ['message'],
        functions: [],
      })
      break
    case 'call':
      void call(message)
      break
    case 'hostResult': {
      const request = pending.get(message.id)
      pending.delete(message.id)
      if (message.ok) request?.resolve(message.value)
      else request?.reject(new Error(message.error))
      break
    }
    case 'shutdown':
      process.exit(0)
      break
  }
}

process.stdin.on('data', (chunk) => {
  buffer = Buffer.concat([buffer, chunk])
  while (buffer.length >= 4) {
    const length = buffer.readUInt32BE()
    if (length > MAX_MESSAGE) throw new Error('Protocol message exceeds 16 MiB')
    if (buffer.length < 4 + length) return
    receive(decode(buffer.subarray(4, 4 + length)))
    buffer = buffer.subarray(4 + length)
  }
})
