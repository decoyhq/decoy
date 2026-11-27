import { spawn } from 'node:child_process'
import { mkdirSync, mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { delimiter, join, resolve } from 'node:path'
import { describe, expect, test } from '@rstest/core'
import { stopProcess, withLocalBins } from './process'

const STUBBORN = resolve(process.cwd(), 'fixtures/stubborn.mjs')

describe('withLocalBins', () => {
  test('puts every node_modules/.bin from cwd up first on PATH', () => {
    const root = mkdtempSync(join(tmpdir(), 'decoy-bins-'))
    const nested = join(root, 'apps', 'web')
    mkdirSync(join(root, 'node_modules', '.bin'), { recursive: true })
    mkdirSync(join(nested, 'node_modules', '.bin'), { recursive: true })

    const env = withLocalBins(nested, { PATH: '/usr/bin' })
    const path = (env.PATH ?? '').split(delimiter)
    expect(path.slice(0, 2)).toEqual([
      join(nested, 'node_modules', '.bin'),
      join(root, 'node_modules', '.bin'),
    ])
    expect(path.at(-1)).toBe('/usr/bin')
  })
})

describe('stopProcess', () => {
  test.skipIf(process.platform === 'win32')(
    'falls back to SIGKILL when SIGTERM is ignored',
    async () => {
      const child = spawn(`node ${STUBBORN}`, { shell: true, detached: true, stdio: 'pipe' })
      await new Promise((ready) => {
        child.stdout.once('data', ready)
      })

      await stopProcess(child, 200)

      // The whole group is gone, not just the shell: `node` under it ignored SIGTERM.
      expect(() => process.kill(-(child.pid ?? 0), 0)).toThrow()
    },
  )

  test('does nothing for a process that already exited', async () => {
    const child = spawn('exit 0', { shell: true, detached: process.platform !== 'win32' })
    await new Promise((done) => {
      child.once('exit', done)
    })
    await stopProcess(child, 200)
  })
})
