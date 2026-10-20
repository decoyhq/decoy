import { once } from 'node:events'
import { createServer, type Server } from 'node:http'
import type { DecoyOptions } from '@decoyhq/testplane'
import type { ConfigInput } from 'testplane'

/** Fixed port of the example's own upstream, targeted by the Passthrough Behavior in mocks/. */
const UPSTREAM_PORT = 4174
let upstream: Server | undefined
// As Playwright does by default: CI runners (Ubuntu 24 AppArmor) block Chrome's sandbox.
const chromeOptions = { args: ['--no-sandbox'] }

export default {
  gridUrl: 'local',
  baseUrl: 'http://127.0.0.1:4173',
  sets: { all: { files: 'tests/**/*.testplane.ts' } },
  // Routing runs over CDP in both session kinds: WebDriver Classic (Testplane's default) and BiDi.
  browsers: {
    chrome: {
      headless: true,
      desiredCapabilities: { browserName: 'chrome', 'goog:chromeOptions': chromeOptions },
    },
    'chrome-bidi': {
      headless: true,
      desiredCapabilities: {
        browserName: 'chrome',
        'goog:chromeOptions': chromeOptions,
        webSocketUrl: true,
      },
    },
  },
  // Testplane's `webServer`: the static app on port 4173.
  devServer: {
    // `exec`: Testplane stops the dev server by signalling its shell only; without `exec` the
    // `node` under that shell would outlive the run and keep port 4173.
    command: 'exec node app/serve.mjs',
    reuseExisting: !process.env.CI,
    readinessProbe: { url: 'http://127.0.0.1:4173' },
  },
  // Master-process hooks: the local Passthrough upstream lives for the whole run.
  beforeAll: async () => {
    upstream = createServer((req, res) => {
      res
        .writeHead(200, { 'content-type': 'application/json' })
        .end(JSON.stringify({ name: `Upstream profile (${req.method} ${req.url})` }))
    })
    upstream.listen(UPSTREAM_PORT, '127.0.0.1')
    await once(upstream, 'listening')
  },
  afterAll: () => {
    upstream?.close()
  },
  plugins: {
    '@decoyhq/testplane': {
      // DECOY_BINARY_PATH: a prebuilt `decoy` (as in CI); otherwise build and run it with cargo.
      command: `${process.env.DECOY_BINARY_PATH ? JSON.stringify(process.env.DECOY_BINARY_PATH) : 'cargo run --quiet -p decoy --'} serve --config mocks/decoy.yaml`,
      urls: '**/api/**',
      collection: 'happy-path',
      // The first run compiles the decoy binary.
      timeout: 180_000,
    } satisfies DecoyOptions,
  },
} satisfies ConfigInput
