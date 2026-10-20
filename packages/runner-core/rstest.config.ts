import { defineConfig } from '@rstest/core'

// `rstest run --coverage` (CI) writes lcov for Codecov to coverage/lcov.info.
export default defineConfig({
  coverage: {
    provider: 'v8',
    reporters: ['text-summary', 'lcov'],
    // Every source file, so ones no test imports count as uncovered instead of vanishing.
    include: ['src/**/*.ts'],
  },
})
