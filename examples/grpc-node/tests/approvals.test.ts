import assert from 'node:assert/strict'
import { after, before, test } from 'node:test'
import * as grpc from '@grpc/grpc-js'
import { approvalsClient, control, getApproval, startDecoy } from './decoy.ts'

let stop: () => void
const client = approvalsClient()

before(async () => {
  stop = await startDecoy()
})

after(() => {
  client.close()
  stop()
})

void test('a Case matches the decoded request message', async () => {
  const reply = await getApproval(client, 'a-1')
  assert.deepEqual(reply, { id: 'a-1', state: 'PENDING', approvers: ['alice'] })
})

void test('templates read the request message and metadata', async () => {
  const reply = await getApproval(client, 'a-7', { 'x-user': 'carol' })
  assert.deepEqual(reply, { id: 'a-7', state: 'PENDING', approvers: ['carol'] })
})

void test('useCollection switches a Session to a gRPC status error', async () => {
  await control('down', 'useCollection', { collection: 'approvals-down' })
  await assert.rejects(getApproval(client, 'a-7', { 'x-mock-session': 'down' }), (error) => {
    const status = error as grpc.ServiceError
    assert.equal(status.code, grpc.status.NOT_FOUND)
    assert.equal(status.details, 'approval a-7 not found')
    assert.deepEqual(status.metadata.get('x-reason'), ['missing'])
    return true
  })
  // Other Sessions keep the happy path.
  const reply = await getApproval(client, 'a-7', { 'x-user': 'dan' })
  assert.equal(reply.state, 'PENDING')
})

void test('useRoute overrides one Behavior for a Session', async () => {
  await control('approved', 'useRoute', {
    route: 'approvals-get',
    case: 'pending',
    behavior: 'approved',
  })
  const reply = await getApproval(client, 'a-1', { 'x-mock-session': 'approved' })
  assert.deepEqual(reply.approvers, ['alice', 'bob'])
  assert.equal(reply.state, 'APPROVED')
})

void test('an unmapped call fails closed with UNIMPLEMENTED', async () => {
  const miss = new grpc.Client('127.0.0.1:8091', grpc.credentials.createInsecure())
  await assert.rejects(
    new Promise((resolve, reject) => {
      miss.makeUnaryRequest(
        '/approvals.v1.Approvals/DeleteApproval',
        (value: Buffer) => value,
        (value: Buffer) => value,
        Buffer.alloc(0),
        (error, reply) => {
          if (error) {
            reject(error)
          } else {
            resolve(reply)
          }
        },
      )
    }),
    (error) => {
      const status = error as grpc.ServiceError
      assert.equal(status.code, grpc.status.UNIMPLEMENTED)
      assert.deepEqual(status.metadata.get('x-mock-miss'), ['true'])
      return true
    },
  )
  miss.close()
})
