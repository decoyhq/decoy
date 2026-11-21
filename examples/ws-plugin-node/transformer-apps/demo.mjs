import assert from 'node:assert/strict'
import { gunzipSync, gzipSync } from 'node:zlib'
import { decode, encode } from '@msgpack/msgpack'
import { Socket, decodeProto, encodeProto, send, startDecoy } from '../tests/decoy.ts'

const SESSION = 'transformer-apps'
const stop = await startDecoy('transformer-apps/decoy.yaml')
const sockets = []

/** Имитирует подписку браузера и обновление статуса, пришедшее после неё. */
async function checkApprovals() {
  const socket = await Socket.open('/ws/approvals', SESSION)
  sockets.push(socket)
  const request = encodeProto('approvals.v1.WatchRequest', { ids: ['expense-42'] })
  const header = Buffer.alloc(5)
  header.writeUInt32BE(request.length, 1)
  socket.send(encode({ meta: { topic: 'watch' }, payload: Buffer.concat([header, request]) }))

  await expectApproval(socket, 'PENDING')
  assert.equal(await send(SESSION, 'approvals', { id: 'expense-42', state: 'APPROVED' }), 1)
  await expectApproval(socket, 'APPROVED')
}

/** Проверяет тему, заголовок gRPC и содержимое ответа браузеру. */
async function expectApproval(socket, state) {
  const { meta, payload } = decode(await socket.binary())
  assert.deepEqual(meta, { topic: 'approval' })
  const frame = Buffer.from(payload)
  assert.equal(frame[0], 0)
  assert.equal(frame.readUInt32BE(1), frame.length - 5)
  assert.deepEqual(decodeProto('approvals.v1.Approval', frame.subarray(5)), {
    id: 'expense-42',
    state,
  })
  console.log(`Approvals dashboard: expense-42 ${state}`)
}

/** Проверяет бинарный gzip-протокол клиента и ответ шлюза с температурой. */
async function checkTelemetry() {
  const socket = await Socket.open('/ws/telemetry', SESSION)
  sockets.push(socket)
  socket.send(new TextEncoder().encode('invalid gzip'))
  assert.ok(await socket.silent())

  socket.send(gzipSync(JSON.stringify({ action: 'subscribe', sensorId: 'freezer-17' })))
  const snapshot = JSON.parse(gunzipSync(await socket.binary()).toString('utf8'))
  assert.deepEqual(snapshot, {
    event: 'snapshot',
    sensorId: 'freezer-17',
    temperatureC: -18.4,
    alarm: false,
  })
  console.log(`Freezer monitoring: ${snapshot.sensorId} ${snapshot.temperatureC} °C`)
}

try {
  await checkApprovals()
  await checkTelemetry()
} finally {
  for (const socket of sockets) socket.close()
  await stop()
}
