import assert from 'node:assert/strict'
import { after, before, test } from 'node:test'
import { DECOY_URL, Socket, control, send, startDecoy } from './decoy.ts'

let stop: () => void
const sockets: Socket[] = []

async function open(service: string, session?: string): Promise<Socket> {
  const socket = await Socket.open(service, session)
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

void test('each socket gets its on: connect message, picked by query', async () => {
  const friend = await open('my-friend-webservice')
  const best = await open('my-best-friend-service')
  assert.equal(await friend.next(), 'ping')
  assert.equal(await best.next(), 'ping')
})

void test('message Cases answer with templates; unmatched messages are skipped', async () => {
  const friend = await open('my-friend-webservice')
  await friend.next() // ping

  friend.send({ id: 7, type: 'get-status' })
  assert.deepEqual(await friend.nextJson(), { id: 7, status: 'online' })

  friend.send({ type: 'nope' })
  assert.ok(await friend.silent(), 'an unmatched message gets no reply')

  friend.send({ id: 8, type: 'subscribe', topic: 'news' })
  assert.deepEqual(await friend.nextJson(), { id: 8, type: 'subscribed', topic: 'news' })
  assert.deepEqual(await friend.nextJson(), { type: 'snapshot', topic: 'news' })
  assert.deepEqual(await friend.nextJson(), { type: 'update', topic: 'news', count: 1 })
})

void test('manual-start: sockets wait until the test sends ping', async () => {
  await control('manual', 'useCollection', { collection: 'manual-start' })
  const friend = await open('my-friend-webservice', 'manual')
  const best = await open('my-best-friend-service', 'manual')
  assert.ok(await friend.silent())

  assert.equal(await send('manual', 'friend-socket', 'ping'), 1)
  assert.equal(await friend.next(), 'ping')
  assert.ok(await best.silent(), 'send reaches only the named Route')
})

void test('useRoute changes the next message on an open socket', async () => {
  const friend = await open('my-friend-webservice', 'offline')
  await friend.next() // ping
  await control('offline', 'useRoute', {
    route: 'friend-socket',
    case: 'status',
    behavior: 'offline',
  })
  friend.send({ id: 1, type: 'get-status' })
  assert.deepEqual(await friend.nextJson(), { id: 1, status: 'offline' })
})

// A bound, so a socket that never closes fails the test instead of hanging the run.
void test('a Behavior flushes ordered messages before closing', { timeout: 5000 }, async () => {
  await control('denied', 'useRoute', {
    route: 'friend-socket',
    case: 'hello',
    behavior: 'unauthorized',
  })
  const friend = await open('my-friend-webservice', 'denied')
  assert.deepEqual(await friend.nextJson(), { type: 'error', reason: 'unauthorized' })
  assert.equal(await friend.next(), 'goodbye')
  const closed = await friend.closed
  assert.equal(closed.code, 4001)
  assert.equal(closed.reason, 'unauthorized')
})

// Decoy waits for the client's close reply: dropping the socket with frames still unread would
// reset it, and the page would see 1006 instead of the code.
void test('a close keeps its code while the page is still sending', { timeout: 5000 }, async () => {
  const friend = await open('my-friend-webservice')
  await friend.next() // ping
  friend.send({ type: 'bye' })
  for (let n = 0; n < 2000; n++) {
    friend.send({ type: 'noise', n })
  }
  const closed = await friend.closed
  assert.equal(closed.code, 4002)
})

void test('an upgrade no ws Route matches gets the HTTP miss', async () => {
  const url = `${DECOY_URL.replace('http', 'ws')}/subscribe?service=unknown`
  const ws = new WebSocket(url)
  const event = await new Promise<Event>((resolve) => {
    ws.addEventListener('error', resolve, { once: true })
    ws.addEventListener('open', resolve, { once: true })
  })
  assert.equal(event.type, 'error')
})
