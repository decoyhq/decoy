import { fileURLToPath } from 'node:url'

export { type ControlCall, startFakeDecoy } from './fake-decoy-server'

/** `node FAKE_DECOY <port>` stands in for `decoy serve`: it answers health on that port. */
export const FAKE_DECOY = fileURLToPath(new URL('./fake-decoy.mjs', import.meta.url))
