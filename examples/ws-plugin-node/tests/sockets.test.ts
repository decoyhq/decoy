import assert from 'node:assert/strict'
import { after, before, test } from 'node:test'
import { decode, encode } from '@msgpack/msgpack'
import { Socket, control, decodeProto, encodeProto, send, startDecoy } from './decoy.ts'

let stop: () => void
const sockets: Socket[] = []
let sessions = 0

/** A fresh Session per test, so tests can change Behaviors without touching each other. */
function session(): string {
  sessions += 1
  return `plugins-${sessions}`
}

async function open(path: string, session: string): Promise<Socket> {
  const socket = await Socket.open(path, session)
  sockets.push(socket)
  return socket
}

before(async () => {
  stop = await startDecoy()
})

after(() => {
  for (const socket of sockets) {
    socket.close()
  }
  stop()
})

// approvals-live: msgpack { type, data: <protobuf> } in binary frames, JSON in text frames.

/** A binary frame as the app sends it: msgpack around a protobuf payload. */
function envelope(type: string, data: object): Uint8Array {
  return encode({ type, data: encodeProto(type, data) })
}

function readEnvelope(frame: Uint8Array): { type: string; data: unknown } {
  const { type, data } = decode(frame) as { type: string; data: Uint8Array }

  return { type, data: decodeProto(type, data) }
}

void test('typed envelopes: JSON text on connect, protobuf replies in binary frames', async () => {
  const approvals = await open('/ws/approvals', session())

  // No `type`: the plugin sends it as JSON text.
  assert.deepEqual(await approvals.json(), { action: 'ping' })

  approvals.send(envelope('approvals.v1.WatchRequest', { ids: ['a-1'] }))

  assert.deepEqual(readEnvelope(await approvals.binary()), {
    type: 'approvals.v1.Approval',
    data: { id: 'a-1', state: 'PENDING' },
  })
  assert.deepEqual(readEnvelope(await approvals.binary()), {
    type: 'approvals.v1.Approval',
    data: { id: 'a-1', state: 'APPROVED' },
  })
})

void test('send goes through the plugin; one that cannot encode answers 400', async () => {
  const id = session()
  const approvals = await open('/ws/approvals', id)
  await approvals.json() // ping

  const update = { type: 'approvals.v1.Approval', data: { id: 'a-1', state: 'REJECTED' } }
  assert.equal(await send(id, 'approvals-live', update), 1)
  assert.deepEqual(readEnvelope(await approvals.binary()), update)

  // `stat` is not a field of Approval: the plugin's encode fails before any socket gets it.
  const typo = { type: 'approvals.v1.Approval', data: { id: 'a-1', stat: 'REJECTED' } }
  const response = await control(id, 'send', { route: 'approvals-live', message: typo })
  assert.equal(response.status, 400)

  const { error } = (await response.json()) as { error: string }
  assert.match(error, /plugin `env: typed-envelope@1\.1\.0` step `envelope` encode failed: .*stat/)
  assert.ok(await approvals.silent(), 'nothing reached the socket')
})

void test('a frame the plugin misses gets nothing, and the socket stays usable', async () => {
  const approvals = await open('/ws/approvals', session())
  await approvals.json() // ping

  approvals.send(encode({ type: 'nobody.v1.Knows', data: new Uint8Array() }))
  assert.ok(await approvals.silent())

  approvals.send(envelope('approvals.v1.WatchRequest', { ids: ['a-1'] }))
  assert.equal(readEnvelope(await approvals.binary()).type, 'approvals.v1.Approval')
})

// rpc: [u8 kind][u16 topic length][topic][u32 id][body], kinds REQUEST 1, REPLY 2, ERROR 3, EVENT 4.

const KIND = { REQUEST: 1, REPLY: 2, ERROR: 3, EVENT: 4 }

function rpcFrame(kind: number, topic: string, id: number, body: Uint8Array): Uint8Array {
  const name = new TextEncoder().encode(topic)
  const frame = new Uint8Array(3 + name.length + 4 + body.length)
  const view = new DataView(frame.buffer)

  frame[0] = kind
  view.setUint16(1, name.length)
  frame.set(name, 3)
  view.setUint32(3 + name.length, id)
  frame.set(body, 7 + name.length)

  return frame
}

function readRpc(frame: Uint8Array): { kind: number; topic: string; id: number; body: Uint8Array } {
  const view = new DataView(frame.buffer, frame.byteOffset, frame.byteLength)
  const end = 3 + view.getUint16(1)

  return {
    kind: frame[0] ?? -1,
    topic: new TextDecoder().decode(frame.subarray(3, end)),
    id: view.getUint32(end),
    body: frame.subarray(end + 4),
  }
}

const getUser = (id: number) =>
  rpcFrame(KIND.REQUEST, 'users.get', id, encodeProto('users.v1.GetUserRequest', { id: 'u-1' }))

void test('rpc frames: a REQUEST gets its REPLY with the same id', async () => {
  const rpc = await open('/ws/rpc', session())

  const welcome = readRpc(await rpc.binary())
  assert.deepEqual([welcome.kind, welcome.topic, welcome.id], [KIND.EVENT, 'users.updated', 1])
  assert.deepEqual(decodeProto('users.v1.User', welcome.body), { id: 'me', name: 'Current User' })

  rpc.send(getUser(7))
  const reply = readRpc(await rpc.binary())
  assert.deepEqual([reply.kind, reply.topic, reply.id], [KIND.REPLY, 'users.get', 7])
  assert.deepEqual(decodeProto('users.v1.User', reply.body), { id: 'u-1', name: 'Ann' })
})

void test('another Session on the same socket path gets the broken Behaviors', async () => {
  const happy = await open('/ws/rpc', session())

  const brokenSession = session()
  const ok = await control(brokenSession, 'useCollection', { collection: 'broken' })
  assert.equal(ok.status, 200)
  const broken = await open('/ws/rpc', brokenSession)

  await happy.binary() // welcome
  await broken.binary()

  happy.send(getUser(1))
  broken.send(getUser(1))
  assert.equal(readRpc(await happy.binary()).kind, KIND.REPLY)

  const error = readRpc(await broken.binary())
  assert.deepEqual([error.kind, error.id], [KIND.ERROR, 1])
  assert.equal(new DataView(error.body.buffer, error.body.byteOffset).getUint16(0), 404)
  assert.equal(new TextDecoder().decode(error.body.subarray(2)), 'no user u-1')
})

void test('the plugin numbers events per connection', async () => {
  const id = session()
  const quiet = await control(id, 'useCollection', { collection: 'manual-start' })
  assert.equal(quiet.status, 200)

  const first = await open('/ws/rpc', id)
  assert.ok(await first.silent(), 'manual-start keeps the open quiet')

  // `wait` counts sockets of a pipeline Route too.
  const wait = await control(id, 'wait', { route: 'rpc', connected: true })
  assert.deepEqual(await wait.json(), { connections: 1 })

  const event = { kind: 'EVENT', topic: 'users.updated', payload: { id: 'u-1', name: 'Ann B.' } }
  await send(id, 'rpc', event)
  await send(id, 'rpc', event)

  assert.equal(readRpc(await first.binary()).id, 1)
  assert.equal(readRpc(await first.binary()).id, 2)

  // A second socket counts from 1; the first keeps counting.
  const second = await open('/ws/rpc', id)
  assert.equal(await send(id, 'rpc', event), 2)

  assert.equal(readRpc(await first.binary()).id, 3)
  assert.equal(readRpc(await second.binary()).id, 1)
})

void test('an unknown kind is a miss; the next request is still answered', async () => {
  const rpc = await open('/ws/rpc', session())
  await rpc.binary() // welcome

  rpc.send(Uint8Array.from([9, 0]))
  assert.ok(await rpc.silent())

  rpc.send(getUser(2))
  assert.equal(readRpc(await rpc.binary()).kind, KIND.REPLY)
})
