import { defineConfig } from '@rstest/core'

// `rstest run --coverage` (CI) writes lcov for Codecov to coverage/lcov.info.
export default defineConfig({
  coverage: { provider: 'v8', reporters: ['text-summary', 'lcov'] },
})
