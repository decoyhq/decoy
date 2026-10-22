// The user's plugin file: an npm plugin, configured in code. mocks/decoy.yaml runs it:
//   command: node ../plugins/binary-rpc.ts
// `export default` is only for unit tests (plugins/binary-rpc.test.ts); Decoy doesn't need it.
import { binaryRpc } from '@acme/decoy-plugin-binary-rpc'

export default binaryRpc({
  topics: {
    'users.get': { request: 'users.v1.GetUserRequest', reply: 'users.v1.User' },
    'users.updated': { event: 'users.v1.User' },
  },
})
