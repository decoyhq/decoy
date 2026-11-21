import { decode, encode } from '@msgpack/msgpack'

const MAX_MESSAGE = 16 * 1024 * 1024
const PROTOCOL = 1
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

const SCHEMAS = new Map([
  ['watch', 'approvals.v1.WatchRequest'],
  ['approval', 'approvals.v1.Approval'],
])

/** Снимает заголовок gRPC и передаёт protobuf-сообщение в Decoy. */
function decodeApproval(bytes) {
  const { meta, payload } = decode(bytes)
  const type = SCHEMAS.get(meta?.topic)
  if (!type || !(payload instanceof Uint8Array)) throw new Error('Expected a known approval topic')
  const frame = Buffer.from(payload)
  if (frame.length < 5 || frame[0] !== 0 || frame.readUInt32BE(1) !== frame.length - 5) {
    throw new Error('Expected one uncompressed gRPC message')
  }
  return host('protobuf.decode', [type, frame.subarray(5)])
}

/** Запрашивает у Decoy сериализацию protobuf и добавляет заголовок gRPC. */
async function encodeApproval(message) {
  const bytes = await host('protobuf.encode', [SCHEMAS.get('approval'), message])
  const payload = Buffer.alloc(5 + bytes.length)
  payload.writeUInt32BE(bytes.length, 1)
  payload.set(bytes, 5)
  return encode({ meta: { topic: 'approval' }, payload })
}

/** Выполняет запрос Decoy, пока цикл чтения принимает ответы на host-вызовы. */
async function call({ id, op, input }) {
  try {
    let value
    switch (op) {
      case 'input':
        value = await decodeApproval(input)
        break
      case 'output':
        value = await encodeApproval(input)
        break
      case 'check':
        await encodeApproval(input)
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
        directions: ['input', 'output'],
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
