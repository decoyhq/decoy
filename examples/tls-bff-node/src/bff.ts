import { createServer } from 'node:http'
import type { AddressInfo } from 'node:net'
import { fileURLToPath } from 'node:url'
import * as grpc from '@grpc/grpc-js'
import * as protoLoader from '@grpc/proto-loader'

/** Where the BFF finds its backends; in this example all three are Decoy. */
export interface Backends {
  /** https */
  paymentsUrl: string
  /** plain http */
  crmUrl: string
  /** gRPC over TLS, `host:port` */
  inventoryAddress: string
}

export interface Bff {
  url: string
  close: () => void
}

interface Stock {
  sku: string
  available: number
}
type StockCallback = (error: grpc.ServiceError | null, reply?: Stock) => void
interface InventoryClient extends grpc.Client {
  getStock(request: { sku: string }, metadata: grpc.Metadata, callback: StockCallback): void
}
interface InventoryPackage {
  inventory: { v1: { Inventory: grpc.ServiceClientConstructor } }
}

const definition = protoLoader.loadSync('inventory/v1/inventory.proto', {
  includeDirs: [fileURLToPath(new URL('../proto', import.meta.url))],
  defaults: true,
})
const { Inventory } = (grpc.loadPackageDefinition(definition) as unknown as InventoryPackage)
  .inventory.v1

/**
 * A tiny BFF: `GET /orders/{id}` joins a payment (https), a customer (http) and stock (gRPC over
 * TLS). Nothing here knows about Decoy except that the `x-mock-session` header is passed on.
 */
export async function startBff(backends: Backends): Promise<Bff> {
  // No custom CA here: Node trusts what NODE_EXTRA_CA_CERTS points at, for fetch and grpc-js alike.
  const inventory = new Inventory(
    backends.inventoryAddress,
    grpc.credentials.createSsl(),
  ) as unknown as InventoryClient

  const server = createServer((request, response) => {
    const id = /^\/orders\/([^/]+)$/.exec(request.url ?? '')?.[1]
    if (!id) {
      response.writeHead(404).end()
      return
    }
    const session = request.headers['x-mock-session']
    const headers: Record<string, string> =
      typeof session === 'string' ? { 'x-mock-session': session } : {}
    const json = async (url: string): Promise<unknown> => {
      const reply = await fetch(url, { headers })
      if (!reply.ok) {
        throw new Error(`${url}: ${reply.status}`)
      }
      return reply.json()
    }
    const metadata = new grpc.Metadata()
    for (const [key, value] of Object.entries(headers)) {
      metadata.set(key, value)
    }
    const stock = new Promise<Stock>((resolve, reject) => {
      inventory.getStock({ sku: `sku-${id}` }, metadata, (error, reply) => {
        if (reply) {
          resolve(reply)
        } else {
          reject(error ?? new Error('empty reply'))
        }
      })
    })

    Promise.all([
      json(`${backends.paymentsUrl}/payments/${id}`),
      json(`${backends.crmUrl}/customers/c-${id}`),
      stock,
    ])
      .then(([payment, customer, stockReply]) => {
        response.writeHead(200, { 'content-type': 'application/json' })
        response.end(JSON.stringify({ id, payment, customer, stock: stockReply }))
      })
      .catch((error: unknown) => {
        response.writeHead(502, { 'content-type': 'text/plain' })
        response.end(String(error))
      })
  })
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
  const { port } = server.address() as AddressInfo
  return {
    url: `http://127.0.0.1:${port}`,
    close: () => {
      inventory.close()
      server.close()
    },
  }
}
