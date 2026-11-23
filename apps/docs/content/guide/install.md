# Install

Decoy is one native binary, `decoy`. Pick the channel that fits your project.

## Install script (standalone)

```sh title="macOS / Linux"
curl -fsSL https://github.com/decoyhq/decoy/releases/latest/download/install.sh | sh
```

```powershell title="Windows PowerShell"
irm 'https://github.com/decoyhq/decoy/releases/latest/download/install.ps1' | iex
```

Installs the release marked Latest to `~/.decoy/bin`, after checking it against the release's
`SHA256SUMS`. Pre-releases are not Latest until promoted. Every release carries its own copy of the
scripts, so pin a version (e.g. in CI) by taking them from that release:
`…/releases/download/v0.1.0-alpha.6/install.sh`. `DECOY_VERSION` or `sh -s -- <version>` also picks
one; `DECOY_INSTALL_DIR` changes the directory. The Windows script adds the directory to your user
`PATH`; on macOS/Linux add it yourself.

Upgrade in place with `decoy upgrade`. It re-runs the Latest install script into the directory of the
running binary, unless the running version is already Latest or a newer pre-release. To move to
another version, run that release's install script with `DECOY_INSTALL_DIR` set instead. If you installed Decoy as a project dependency, update it with your package manager instead.

## Install in a JavaScript or TypeScript project

Add `@decoyhq/cli` as a development dependency in your project directory.

```sh
npm i -D @decoyhq/cli
```

`@decoyhq/cli` installs only the binary for your platform (linux x64/arm64, macOS x64/arm64, Windows
x64) through an optional dependency. No install scripts run. For a pre-release, use `@decoyhq/cli@next` as the package name.

To confirm the install, run `npm exec -- decoy --version`, `pnpm exec decoy --version`, `yarn decoy --version`, or `bun run decoy --version` from the project directory.

## GitHub Releases

Every release has archives for the same platforms plus `SHA256SUMS`:
`decoy-<version>-<target>.tar.gz` (`.zip` on Windows). Unpack and put `decoy` on your `PATH`.
Linux binaries are built against musl and use mimalloc, which is much faster under concurrent load
than musl's allocator but holds more memory (about 30 MB idle).

## From source

```sh title="Build the Decoy binary"
cargo build --release -p decoy
```

Check the build with `./target/release/decoy --version` (`.exe` on Windows). To make `@decoyhq/cli` run this binary, set `DECOY_BINARY_PATH` to its full path.
