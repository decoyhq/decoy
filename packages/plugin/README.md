# Transformer SDK for TypeScript

Use the SDK to write a Transformer that takes a value and returns the value the next Pipeline step needs. A worker can reshape an object, turn bytes into text, or produce bytes in a format your application understands.

The example below handles two WebSocket payload formats: JSON text and binary MessagePack envelopes containing protobuf data. See the [Pipeline guide](../../apps/docs/content/guide/transformer-pipelines.md) for configuration.

```ts
// plugins/envelope.ts
import { decode, encode } from '@msgpack/msgpack'
import { definePlugin, miss } from '@decoyhq/plugin'

export default definePlugin({
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

```yaml
# decoy.yaml
transformers:
  env:
    worker: [node, ./plugins/envelope.ts]
pipelines:
  approvals-ws:
    input: [env]
    output: [env]
```

- `definePlugin` returns the worker definition, and serves it over stdin/stdout when Decoy started the process
  (`DECOY_PLUGIN=1`). A unit test can import the file and call `input` directly.
- A package can export a factory that takes typed options and returns `definePlugin(...)`; users
  call it from their own worker file.
- `miss(reason)` in `input` skips the frame: Decoy logs the reason and keeps the socket open. Any
  other throw is a transformation error, shown with the Transformer, Behavior and value that failed.
- `ctx.frame` is `'text'` or `'binary'` on WebSocket input; `ctx.connection` is `{ id, route, session }`;
  `ctx.state` is a `Map` kept per connection.
- `ctx.protobuf.has/decode/encode` use the protos Decoy already compiled.
- `console.log` goes to stderr while serving, since stdout carries the protocol. Decoy prints each
  stderr line with the worker's name.

To implement a worker in another language, see the [wire protocol](https://github.com/decoyhq/decoy/blob/main/docs/reference/plugin-protocol.md).
