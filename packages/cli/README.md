# @decoy/cli

The `decoy` command: a portable local mock tool.

```sh
npm i -D @decoy/cli
npx decoy serve --config mocks/decoy.yaml
```

This package is a small launcher. npm installs only the matching `@decoy/cli-<os>-<arch>` package
(linux x64/arm64, macOS x64/arm64, Windows x64), which holds the native binary; no install scripts
run. Set `DECOY_BINARY_PATH` to use another build, e.g. one from `cargo build -p decoy-runtime`.
