import type { DecoyTestOptions } from '@decoyhq/playwright'
import { defineConfig, devices } from '@playwright/test'

export default defineConfig<DecoyTestOptions>({
  testDir: './tests',
  globalSetup: './global-setup.ts',
  // Spec files run concurrently against one Decoy runtime; each test's Session keeps it isolated.
  fullyParallel: true,
  // At least 2, so the WebSocket isolation tests can run at once (they fail otherwise).
  workers: process.env.CI ? 2 : undefined,
  forbidOnly: !!process.env.CI,
  reporter: process.env.CI ? 'list' : 'line',
  webServer: {
    command: 'node app/serve.mjs',
    url: 'http://127.0.0.1:4173',
    reuseExistingServer: !process.env.CI,
  },
  use: {
    baseURL: 'http://127.0.0.1:4173',
    decoyOptions: {
      // DECOY_BINARY_PATH: a prebuilt `decoy` (as in CI); otherwise build and run it with cargo.
      command: `${process.env.DECOY_BINARY_PATH ? JSON.stringify(process.env.DECOY_BINARY_PATH) : 'cargo run --quiet -p decoy --'} serve --config mocks/decoy.yaml`,
      urls: '**/api/**',
      webSocketUrls: '**/live', // the page's socket, bridged to Decoy under the test's Session
      collection: 'happy-path',
      // The first run compiles the decoy binary.
      timeout: 180_000,
    },
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
})
