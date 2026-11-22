# @decoyhq/plugin

Write Decoy plugin steps in TypeScript. A plugin turns an app's own wire format (msgpack envelopes,
binary headers in front of protobuf, compression) into the JSON that Cases match and Behaviors
render, and back. See the [WebSocket Transformer guide](https://github.com/decoyhq/decoy/blob/main/docs/guide/websocket.md#transformers) for configuration.

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

- `definePlugin` returns the plugin, and serves it over stdin/stdout when Decoy started the process
  (`DECOY_PLUGIN=1`). A unit test can import the file and call `input` directly.
- A package can export a factory that takes typed options and returns `definePlugin(...)`; users
  call it from their own plugin file.
- On WebSockets, `miss(reason)` in `input` skips the frame: Decoy logs the reason and keeps the socket open. Any
  other throw is a plugin error, shown with the Transformer, Behavior and value that failed.
- WebSocket input sets `ctx.frame` to `'text'` or `'binary'`. WebSocket input and output supply
  `ctx.connection` as `{ id, route, session }` and share a `ctx.state` map per connection. HTTP,
  gRPC and startup checks get a fresh state map for each call.
- HTTP workers receive `ctx.request` on input and output, including Passthrough output. It is a
  read-only snapshot of the original method, path, and headers (lowercase names). The headers can
  include cookies and credentials; avoid logging them. Startup checks have no request context.
- `ctx.protobuf.has/decode/encode` use the protos Decoy already compiled.
- `console.log` goes to stderr while serving, since stdout carries the protocol. Decoy prints each
  stderr line with the plugin's name.

To implement a worker in another language, see the [wire protocol](https://github.com/decoyhq/decoy/blob/main/docs/reference/plugin-protocol.md).
