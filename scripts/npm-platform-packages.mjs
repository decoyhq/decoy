// Turn release archives into @decoyhq/cli-<os>-<arch> npm packages, and pin them as
// optionalDependencies of packages/cli (done at publish time: they only exist once released).
//   node scripts/npm-platform-packages.mjs <archives-dir> <out-dir>
import { fileURLToPath } from 'node:url'
import { execFileSync } from 'node:child_process'
import { chmodSync, cpSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const TARGETS = {
  'x86_64-unknown-linux-musl': { os: 'linux', cpu: 'x64' },
  'aarch64-unknown-linux-musl': { os: 'linux', cpu: 'arm64' },
  'x86_64-apple-darwin': { os: 'darwin', cpu: 'x64' },
  'aarch64-apple-darwin': { os: 'darwin', cpu: 'arm64' },
  'x86_64-pc-windows-msvc': { os: 'win32', cpu: 'x64' },
}

const [archives, out] = process.argv.slice(2)
if (!archives || !out) {
  console.error('usage: node scripts/npm-platform-packages.mjs <archives-dir> <out-dir>')
  process.exit(1)
}
const root = fileURLToPath(new URL('..', import.meta.url))
const cliManifestPath = join(root, 'packages/cli/package.json')
const cli = JSON.parse(readFileSync(cliManifestPath, 'utf8'))
const optionalDependencies = {}

for (const [target, { os, cpu }] of Object.entries(TARGETS)) {
  const name = `@decoyhq/cli-${os}-${cpu}`
  const exe = os === 'win32' ? 'decoy.exe' : 'decoy'
  const archive = join(
    archives,
    `decoy-${cli.version}-${target}.${os === 'win32' ? 'zip' : 'tar.gz'}`,
  )
  const unpacked = mkdtempSync(join(tmpdir(), 'decoy-'))
  execFileSync(
    os === 'win32' ? 'unzip' : 'tar',
    os === 'win32' ? ['-q', archive, '-d', unpacked] : ['-xzf', archive, '-C', unpacked],
  )

  const dir = join(out, `cli-${os}-${cpu}`)
  mkdirSync(join(dir, 'bin'), { recursive: true })
  cpSync(join(unpacked, `decoy-${cli.version}-${target}`, exe), join(dir, 'bin', exe))
  chmodSync(join(dir, 'bin', exe), 0o755)
  writeFileSync(
    join(dir, 'package.json'),
    `${JSON.stringify(
      {
        name,
        version: cli.version,
        description: `The decoy binary for ${os}-${cpu}. Install @decoyhq/cli instead.`,
        license: cli.license,
        homepage: cli.homepage,
        repository: cli.repository,
        os: [os],
        cpu: [cpu],
        files: ['bin'],
      },
      null,
      2,
    )}\n`,
  )
  optionalDependencies[name] = cli.version
  console.log(`${name}@${cli.version} → ${dir}`)
}

writeFileSync(cliManifestPath, `${JSON.stringify({ ...cli, optionalDependencies }, null, 2)}\n`)
