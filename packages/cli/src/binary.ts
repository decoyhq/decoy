import { dirname, join } from 'node:path'

/** `<process.platform>-<process.arch>` → the npm package that ships that `decoy` binary. */
export const PLATFORM_PACKAGES: Readonly<Record<string, string>> = {
  'linux-x64': '@decoyhq/cli-linux-x64',
  'linux-arm64': '@decoyhq/cli-linux-arm64',
  'darwin-x64': '@decoyhq/cli-darwin-x64',
  'darwin-arm64': '@decoyhq/cli-darwin-arm64',
  'win32-x64': '@decoyhq/cli-win32-x64',
}

export interface BinaryLookup {
  platform: string
  arch: string
  env: Readonly<Record<string, string | undefined>>
  /** `require.resolve`-style lookup of a package file; throws when the package is missing. */
  resolve: (id: string) => string
}

/** Path of the native `decoy` binary to run, or a clear error saying how to get one. */
export function findBinary({ platform, arch, env, resolve }: BinaryLookup): string {
  const override = env.DECOY_BINARY_PATH
  if (override) {
    return override
  }
  const key = `${platform}-${arch}`
  const packageName = PLATFORM_PACKAGES[key]
  if (packageName === undefined) {
    throw new Error(
      `@decoyhq/cli has no prebuilt decoy binary for ${key}. Supported: ${Object.keys(PLATFORM_PACKAGES).join(', ')}. ` +
        'Build it from source (cargo build -p decoy-runtime) and set DECOY_BINARY_PATH.',
    )
  }
  let manifest: string
  try {
    manifest = resolve(`${packageName}/package.json`)
  } catch {
    throw new Error(
      `@decoyhq/cli could not find ${packageName}, which holds the decoy binary for ${key}. ` +
        'It is an optional dependency: reinstall without --no-optional / --omit=optional.',
    )
  }
  return join(dirname(manifest), 'bin', platform === 'win32' ? 'decoy.exe' : 'decoy')
}
