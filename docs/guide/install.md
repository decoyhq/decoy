# Install

Decoy is one native binary, `decoy`. Pick the channel that fits your project.

## npm (JS/TS projects)

```sh
npm i -D @decoy/cli
npx decoy --version
```

`@decoy/cli` installs only the binary for your platform (linux x64/arm64, macOS x64/arm64, Windows
x64) through an optional dependency. No install scripts run. Pre-releases are on the `next` tag:
`npm i -D @decoy/cli@next`.

## GitHub Releases

Every release has archives for the same platforms plus `SHA256SUMS`:
`decoy-<version>-<target>.tar.gz` (`.zip` on Windows). Unpack and put `decoy` on your `PATH`.

## From source

```sh
cargo build --release -p decoy-runtime
./target/release/decoy --version
```

To make `@decoy/cli` run a source build, set `DECOY_BINARY_PATH=/path/to/decoy`.
