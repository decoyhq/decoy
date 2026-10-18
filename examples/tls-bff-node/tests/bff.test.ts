import assert from 'node:assert/strict'
import { after, before, test } from 'node:test'
import { type Bff, startBff } from '../src/bff.ts'

const DECOY_HTTPS = 'https://localhost:8443'
const DECOY_HTTP = 'http://127.0.0.1:8092'

let bff: Bff

before(async () => {
  bff = await startBff({
    paymentsUrl: DECOY_HTTPS,
    crmUrl: DECOY_HTTP,
    inventoryAddress: 'localhost:8443',
  })
})

after(() => {
  bff.close()
})

async function order(id: string, session?: string): Promise<Record<string, unknown>> {
  const response = await fetch(`${bff.url}/orders/${id}`, {
    headers: session ? { 'x-mock-session': session } : {},
  })
  assert.equal(response.status, 200, await response.clone().text())
  return (await response.json()) as Record<string, unknown>
}

void test('the BFF reaches an https backend, an http backend and gRPC over TLS', async () => {
  assert.deepEqual(await order('7'), {
    id: '7',
    payment: { id: '7', status: 'paid', amount: 42 },
    customer: { id: 'c-7', name: 'Ada' },
    stock: { sku: 'sku-7', available: 3 },
  })
})

void test('one Session switches every listener, https and http alike', async () => {
  // The Control API answers on any listener; here the plain one.
  const response = await fetch(`${DECOY_HTTP}/__decoy__/control/useCollection`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', 'x-mock-session': 'empty' },
    body: JSON.stringify({ collection: 'out-of-stock' }),
  })
  assert.equal(response.status, 200)

  assert.deepEqual((await order('7', 'empty')).stock, { sku: 'sku-7', available: 0 })
  assert.deepEqual((await order('7')).stock, { sku: 'sku-7', available: 3 })
})

void test('the https backend serves the generated localhost cert', async () => {
  // Trusted only because NODE_EXTRA_CA_CERTS names mocks/.decoy/ca.pem.
  const response = await fetch(`${DECOY_HTTPS}/__decoy__/health`)
  assert.equal(response.status, 200)
})
