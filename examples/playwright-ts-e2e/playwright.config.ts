import type { DecoyTestOptions } from '@decoyhq/playwright'
import { defineConfig, devices } from '@playwright/test'

export default defineConfig<DecoyTestOptions>({
  testDir: './tests',
  globalSetup: './global-setup.ts',
  // Spec files run concurrently against one Decoy runtime; each test's Session keeps it isolated.
  fullyParallel: true,
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
      command: 'cargo run --quiet -p decoy-runtime -- serve --config mocks/decoy.yaml',
      urls: '**/api/**',
      collection: 'happy-path',
      // The first run compiles the decoy binary.
      timeout: 180_000,
    },
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
})
