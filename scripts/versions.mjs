// One version for the decoy binary and every published npm package.
//   node scripts/versions.mjs check          fail if any version differs from Cargo.toml
//   node scripts/versions.mjs set <version>  rewrite them all (used by `task release`)
import { fileURLToPath } from 'node:url'
import { execFileSync } from 'node:child_process'
import { readdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

const root = fileURLToPath(new URL('..', import.meta.url))
const cargoToml = join(root, 'Cargo.toml')
const WORKSPACE_VERSION = /(\[workspace\.package\][^[]*?\nversion = ")([^"]+)(")/
const SEMVER = /^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/

function cargoVersion() {
  const match = WORKSPACE_VERSION.exec(readFileSync(cargoToml, 'utf8'))
  if (!match) {
    throw new Error('Cargo.toml has no [workspace.package] version')
  }
  return match[2]
}

function publishedPackages() {
  const dir = join(root, 'packages')
  return readdirSync(dir)
    .map((name) => join(dir, name, 'package.json'))
    .map((path) => ({ path, json: JSON.parse(readFileSync(path, 'utf8')) }))
    .filter(({ json }) => !json.private)
}

const [command, version] = process.argv.slice(2)

if (command === 'check') {
  const expected = cargoVersion()
  const drift = publishedPackages().filter(({ json }) => json.version !== expected)
  for (const { path, json } of drift) {
    console.error(`${path}: version ${json.version}, expected ${expected} (Cargo.toml)`)
  }
  process.exit(drift.length === 0 ? 0 : 1)
} else if (command === 'set' && version !== undefined && SEMVER.test(version)) {
  writeFileSync(
    cargoToml,
    readFileSync(cargoToml, 'utf8').replace(WORKSPACE_VERSION, `$1${version}$3`),
  )
  for (const { path, json } of publishedPackages()) {
    writeFileSync(path, `${JSON.stringify({ ...json, version }, null, 2)}\n`)
  }
  execFileSync('cargo', ['update', '--workspace', '--offline'], { cwd: root, stdio: 'inherit' })
  console.log(`version ${version}`)
} else {
  console.error('usage: node scripts/versions.mjs check | set <semver>')
  process.exit(1)
}
