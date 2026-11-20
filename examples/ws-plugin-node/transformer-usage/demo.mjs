import assert from 'node:assert/strict'
import { decode, encode } from '@msgpack/msgpack'
import { Socket, decodeProto, encodeProto, startDecoy } from '../tests/decoy.ts'

const WATCH = ['watch', 'approvals.v1.WatchRequest', { ids: ['a-1'] }]
const LOOKUP = ['lookup', 'users.v1.GetUserRequest', { id: 'a-1' }]
const SCENARIOS = [
  [
    'approval-command',
    WATCH,
    'approval',
    'approvals.v1.Approval',
    { id: 'a-1', state: 'APPROVED' },
  ],
  [
    'approval-command',
    LOOKUP,
    'approval',
    'approvals.v1.Approval',
    { id: 'a-1', state: 'PENDING' },
  ],
  ['approval-chain', WATCH, 'approval', 'approvals.v1.Approval', { id: 'a-1', state: 'APPROVED' }],
  ['approval-chain', LOOKUP, 'approval', 'approvals.v1.Approval', { id: 'a-1', state: 'PENDING' }],
  ['user-native', LOOKUP, 'user', 'users.v1.User', { id: 'a-1', name: 'Ada' }],
]

/** Пакует запрос так же, как это делает WebSocket-клиент. */
function pack([topic, schema, message]) {
  const body = encodeProto(schema, message)
  const header = Buffer.alloc(5)
  header.writeUInt32BE(body.length, 1)
  return encode({ meta: { topic }, payload: Buffer.concat([header, body]) })
}

const worker = process.argv[2] === 'worker'
const config = worker ? 'worker.yaml' : 'decoy.yaml'
const scenarios = worker
  ? SCENARIOS.slice(0, 2).map(([, ...scenario]) => ['approval-worker', ...scenario])
  : SCENARIOS
const stop = await startDecoy(`transformer-usage/${config}`)
const sockets = new Map()
try {
  for (const [route, request, topic, schema, expected] of scenarios) {
    if (!sockets.has(route)) {
      sockets.set(route, await Socket.open(`/ws/${route}`, 'transformer-demo'))
    }
    const socket = sockets.get(route)
    socket.send(pack(request))
    const result = decode(await socket.binary())
    assert.deepEqual(result.meta, { topic })
    const payload = Buffer.from(result.payload)
    assert.equal(payload[0], 0)
    assert.equal(payload.readUInt32BE(1), payload.length - 5)
    const message = decodeProto(schema, payload.subarray(5))
    assert.deepEqual(message, expected)
    console.log(`${route} · ${request[0]} → ${topic}`, message)
  }
} finally {
  for (const socket of sockets.values()) socket.close()
  stop()
}
