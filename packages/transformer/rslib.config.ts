import { defineConfig } from '@rslib/core'

// Each format ships its own declarations (`.d.ts` for ESM, `.d.cts` for CJS) with explicit
// extensions, so `moduleResolution: nodenext` consumers resolve them in either mode.
// Bundle each format separately: native unbundled emit races on the shared .d.ts files.
const dts = { autoExtension: true, bundle: true }
const redirect = { dts: { extension: true } }

export default defineConfig({
  source: { entry: { index: './src/index.ts' }, tsconfigPath: './tsconfig.build.json' },
  lib: [
    { format: 'esm', syntax: 'es2023', dts, redirect },
    { format: 'cjs', syntax: 'es2023', dts, redirect },
  ],
})
