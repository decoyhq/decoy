# @decoyhq/transformer

Write Decoy Transformer workers in TypeScript. A worker can turn an app's wire format (msgpack
envelopes, binary headers in front of protobuf, compression) into the JSON that Cases match and
Behaviors render, and back. HTTP, gRPC, and WebSocket Routes can use workers. See [Configure a Transformer Pipeline](../transformer-pipelines/index.md) for setup.

Install the SDK and the MessagePack library used in this example (Node.js 24+):

```sh title="Install worker dependencies"
npm install @decoyhq/transformer @msgpack/msgpack
```

This example adapts binary and text WebSocket messages. Save it as `workers/envelope.ts`:

```ts title="workers/envelope.ts"
import { decode, encode } from '@msgpack/msgpack'
import { defineTransformer, miss } from '@decoyhq/transformer'

export default defineTransformer({
  name: 'envelope',
  version: '1.0.0',

  async input(bytes, ctx) {
    if (!(bytes instanceof Uint8Array)) return bytes
    if (ctx.frame === 'text') return JSON.parse(new TextDecoder().decode(bytes))

    const { type, data } = decode(bytes) as { type: string; data: Uint8Array }
    if (!(await ctx.protobuf.has(type))) miss(`unknown type ${type}`)
    return { type, data: await ctx.protobuf.decode(type, data) }
  },

  async output(value, ctx) {
    if (typeof value !== 'object' || value === null || !('type' in value)) {
      return JSON.stringify(value) // a string goes out as a text frame
    }

    const { type, data } = value as { type: string; data: unknown }
    return encode({ type, data: await ctx.protobuf.encode(type, data) }) // bytes: binary
  },
})
```

Register the worker and name its Pipeline in `decoy.yaml`:

```yaml title="decoy.yaml (excerpt)"
transformers:
  env:
    worker: [node, ./workers/envelope.ts]

pipelines:
  approvals-ws:
    input: [env]
    output: [env]
```

- `defineTransformer` returns the transformer, and serves it over stdin/stdout when Decoy started the process
  (`DECOY_TRANSFORMER=1`). A unit test can import the file and call `input` directly.
- A package can export a factory that takes typed options and returns `defineTransformer(...)`; users
  call it from their own worker file.
- On WebSockets, `miss(reason)` in `input` skips the frame: Decoy logs the reason and keeps the socket open. Any
  other throw is a worker error, shown with the Transformer, Behavior and value that failed.
- WebSocket input sets `ctx.frame` to `'text'` or `'binary'`. WebSocket input and output supply
  `ctx.connection` as `{ id, route, session }` and share a `ctx.state` map per connection. HTTP,
  gRPC and startup checks get a fresh state map for each call.
- HTTP workers receive `ctx.request` on input and output, including Passthrough output. It is a
  read-only snapshot of the original method, path, and headers (lowercase names). The headers can
  include cookies and credentials; avoid logging them. Startup checks have no request context.
- `ctx.protobuf.has/decode/encode` use the protos Decoy already compiled.
- `console.log` goes to stderr while serving, since stdout carries the protocol. Decoy prints each
  stderr line with the worker's label.

To implement a worker in another language, see the [wire protocol](../transformer-worker-protocol/index.md).
