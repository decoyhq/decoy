# @decoyhq/plugin

Write Decoy plugin steps in TypeScript. A plugin turns an app's own wire format (msgpack envelopes,
binary headers in front of protobuf, compression) into the JSON that Cases match and Behaviors
render, and back. Background: `docs/design/transports.md` §7.

```ts
// plugins/envelope.ts
import { decode, encode } from '@msgpack/msgpack'
import { definePlugin, miss } from '@decoyhq/plugin'

export default definePlugin({
  name: 'envelope',
  version: '1.0.0',
  steps: {
    envelope: {
      async decode(bytes, ctx) {
        if (ctx.frame === 'text') return JSON.parse(new TextDecoder().decode(bytes))
        const { type, data } = decode(bytes) as { type: string; data: Uint8Array }
        if (!(await ctx.protobuf.has(type))) miss(`unknown type ${type}`)
        return { type, data: await ctx.protobuf.decode(type, data) }
      },
      async encode(value, ctx) {
        if (typeof value !== 'object' || value === null || !('type' in value)) {
          return JSON.stringify(value) // a string goes out as a text frame
        }
        const { type, data } = value as { type: string; data: unknown }
        return encode({ type, data: await ctx.protobuf.encode(type, data) }) // bytes: binary
      },
    },
  },
})
```

```yaml
# decoy.yaml
plugins:
  env:
    command: node ./plugins/envelope.ts
pipelines:
  approvals-ws: [env/envelope]
```

- `definePlugin` returns the plugin, and serves it over stdin/stdout when Decoy started the process
  (`DECOY_PLUGIN=1`). A unit test can import the file and call `steps.envelope.decode` directly.
- A package can export a factory that takes typed options and returns `definePlugin(...)`; users
  call it from their own plugin file.
- `miss(reason)` in `decode` skips the frame: Decoy logs the reason and keeps the socket open. Any
  other throw is a plugin error, shown with the step, Behavior and value that failed.
- `ctx.frame` is `'text'` or `'binary'` while decoding; `ctx.connection` is `{ id, route, session }`;
  `ctx.state` is a `Map` kept per connection.
- `ctx.protobuf.has/decode/encode` use the protos Decoy already compiled.
- `console.log` goes to stderr while serving, since stdout carries the protocol. Decoy prints each
  stderr line with the plugin's name.

Other languages: speak `docs/design/plugin-protocol.md` directly.
