import { defineConfig } from '@rslib/core'

export default defineConfig({
  source: { entry: { bin: './src/bin.ts' }, tsconfigPath: './tsconfig.build.json' },
  lib: [{ format: 'esm', syntax: 'es2023', banner: { js: '#!/usr/bin/env node' } }],
})
