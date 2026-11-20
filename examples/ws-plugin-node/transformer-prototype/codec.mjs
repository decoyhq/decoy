import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { decode, encode } from '@msgpack/msgpack'
import protobuf from 'protobufjs'

/** Загружает схемы и собирает операции для двух направлений. */
export function createCodec(schemaMap) {
  const types = JSON.parse(readFileSync(schemaMap, 'utf8'))
  const root = new protobuf.Root()
  let loaded = false

  /** Определяет имя protobuf-схемы по теме протокола. */
  function schemaFor(topic) {
    if (!Object.hasOwn(types, topic)) throw new Error(`Unknown topic: ${topic}`)
    return types[topic]
  }

  /** Возвращает локальную protobuf-схему, загружая дескрипторы при первом вызове. */
  function typeFor(topic) {
    if (!loaded) {
      root.loadSync([
        fileURLToPath(new URL('../proto/approvals/v1/approvals.proto', import.meta.url)),
        fileURLToPath(new URL('../proto/users/v1/users.proto', import.meta.url)),
      ])
      loaded = true
    }
    return root.lookupType(schemaFor(topic))
  }

  /** Извлекает protobuf из одного gRPC-сообщения без сжатия. */
  function unwrap(bytes) {
    const frame = Buffer.from(bytes)
    if (frame.length < 5 || frame[0] !== 0 || frame.readUInt32BE(1) !== frame.length - 5) {
      throw new Error('Expected one uncompressed gRPC message')
    }
    return frame.subarray(5)
  }

  /** Добавляет gRPC-заголовок к protobuf. */
  function wrap(bytes) {
    const frame = Buffer.alloc(5 + bytes.length)
    frame.writeUInt32BE(bytes.length, 1)
    frame.set(bytes, 5)
    return frame
  }

  /** Разбирает промежуточное значение, сохраняя тему и исходные байты. */
  function envelope(bytes) {
    const value = decode(bytes)
    if (typeof value?.meta?.topic !== 'string' || !(value.payload instanceof Uint8Array)) {
      throw new Error('Expected meta.topic and binary payload')
    }
    return value
  }

  /** Разбирает прикладное сообщение после снятия gRPC-заголовка. */
  function readMessage({ meta, payload }) {
    const type = typeFor(meta.topic)
    return type.toObject(type.decode(payload), { enums: String })
  }

  /** Сериализует прикладное сообщение и добавляет метаданные протокола. */
  function writeMessage(message, topic) {
    const type = typeFor(topic)
    return { meta: { topic }, payload: type.encode(type.fromObject(message)).finish() }
  }

  return {
    inboundHost(bytes, proto) {
      const { meta, payload } = envelope(bytes)
      return proto.decode(schemaFor(meta.topic), unwrap(payload))
    },
    async outboundHost(message, topic, proto) {
      const bytes = await proto.encode(schemaFor(topic), message)
      return encode({ meta: { topic }, payload: wrap(bytes) })
    },
    inbound(bytes) {
      const { meta, payload } = envelope(bytes)
      return readMessage({ meta, payload: unwrap(payload) })
    },
    outbound(message, topic) {
      const { meta, payload } = writeMessage(message, topic)
      return encode({ meta, payload: wrap(payload) })
    },
    stripGrpc(bytes) {
      const { meta, payload } = envelope(bytes)
      return encode({ meta, payload: unwrap(payload) })
    },
    readProto(bytes) {
      return readMessage(envelope(bytes))
    },
    writeProto: writeMessage,
    addGrpc(bytes) {
      const { meta, payload } = envelope(bytes)
      return encode({ meta, payload: wrap(payload) })
    },
  }
}
