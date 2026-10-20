export { createRouter, type DecoyRouter, SESSION_HEADER } from './control'
export { DEFAULT_URL, HEALTH_PATH, isHealthy } from './health'
export { stopProcess, withLocalBins } from './process'
export {
  type ResolvedServerOptions,
  resolveServerOptions,
  type ServerOptions,
  startDecoy,
  type Teardown,
} from './server'
