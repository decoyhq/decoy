# @decoyhq/transformer

Write Decoy Transformer workers in TypeScript. Workers can decode transport payloads before Case matching and encode the values Behaviors produce.

```sh
npm install @decoyhq/transformer
```

```ts
import { defineTransformer } from '@decoyhq/transformer'

export default defineTransformer({
  name: 'json-input',
  version: '1.0.0',
  input: (bytes) =>
    bytes instanceof Uint8Array ? JSON.parse(new TextDecoder().decode(bytes)) : bytes,
})
```

See the [Transformer SDK guide](https://usedecoy.dev/docs/transformer-sdk/) for install commands, worker setup, Pipeline configuration, and a complete example.
