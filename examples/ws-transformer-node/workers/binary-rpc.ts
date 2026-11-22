// The user's transformer file: an npm transformer, configured in code. mocks/decoy.yaml runs it:
//   command: node ../workers/binary-rpc.ts
// `export default` is only for unit tests (workers/binary-rpc.test.ts); Decoy doesn't need it.
import { binaryRpc } from '@acme/decoy-transformer-binary-rpc'

export default binaryRpc({
  topics: {
    'users.get': { request: 'users.v1.GetUserRequest', reply: 'users.v1.User' },
    'users.updated': { event: 'users.v1.User' },
  },
})
