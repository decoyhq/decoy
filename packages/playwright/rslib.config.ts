import { defineConfig } from '@rslib/core'

export default defineConfig({
  source: { entry: { index: './src/index.ts' }, tsconfigPath: './tsconfig.build.json' },
  lib: [
    { format: 'esm', syntax: 'es2023', dts: true },
    { format: 'cjs', syntax: 'es2023' },
  ],
})
