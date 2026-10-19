import { join } from 'node:path'
import { describe, expect, test } from '@rstest/core'
import { findBinary } from './binary'

const installed = (id: string) => `/node_modules/${id}`
const missing = (id: string): string => {
  throw new Error(`Cannot find module '${id}'`)
}

describe('findBinary', () => {
  test('uses the platform package binary', () => {
    expect(findBinary({ platform: 'linux', arch: 'arm64', env: {}, resolve: installed })).toBe(
      join('/node_modules/@decoyhq/cli-linux-arm64/bin/decoy'),
    )
  })

  test('uses decoy.exe on Windows', () => {
    expect(findBinary({ platform: 'win32', arch: 'x64', env: {}, resolve: installed })).toBe(
      join('/node_modules/@decoyhq/cli-win32-x64/bin/decoy.exe'),
    )
  })

  test('DECOY_BINARY_PATH overrides the lookup', () => {
    expect(
      findBinary({
        platform: 'freebsd',
        arch: 'x64',
        env: { DECOY_BINARY_PATH: '/src/target/debug/decoy' },
        resolve: missing,
      }),
    ).toBe('/src/target/debug/decoy')
  })

  test('names supported platforms when there is no prebuilt binary', () => {
    expect(() =>
      findBinary({ platform: 'freebsd', arch: 'x64', env: {}, resolve: installed }),
    ).toThrow(
      /no prebuilt decoy binary for freebsd-x64\. Supported: linux-x64, .*DECOY_BINARY_PATH/,
    )
  })

  test('names the missing optional package', () => {
    expect(() =>
      findBinary({ platform: 'darwin', arch: 'arm64', env: {}, resolve: missing }),
    ).toThrow(/could not find @decoyhq\/cli-darwin-arm64.*--omit=optional/)
  })
})
