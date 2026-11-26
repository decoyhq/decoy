"""Select GitHub Actions jobs from the diff of the tested PR merge commit."""

import json
import os
import subprocess
from pathlib import Path, PurePosixPath

JS_SOURCE = {'.ts', '.tsx', '.mts', '.cts', '.js', '.jsx', '.mjs', '.cjs'}
FORMAT_TYPES = JS_SOURCE | {
    '.css', '.flow', '.gql', '.graphql', '.hbs', '.html', '.json', '.json5', '.jsonc',
    '.less', '.md', '.mdx', '.scss', '.vue', '.yaml', '.yml',
}
RUST_CHECK_CONFIGS = {'deny.toml', 'rustfmt.toml', '.rustfmt.toml', 'clippy.toml', '.clippy.toml'}


def changed_paths():
    # Use the base parent, not the PR head: CI checks the result of merging.
    diff = subprocess.check_output(['git', 'diff', '--name-only', '-z', '--no-renames', 'HEAD^1', 'HEAD'])
    return os.fsdecode(diff).split('\0')[:-1]


def classify(paths, *, full=False):
    paths = set(paths)
    docs = {path for path in paths if PurePosixPath(path).suffix.lower() in {'.md', '.mdx'}}
    files = paths - docs
    shared = '.prototools' in files
    rust_inputs = shared or any(path.startswith(('crates/', '.cargo/')) for path in files) or bool(
        files & {'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain'}
    )
    js_inputs = shared or bool(files & {
        'package.json', 'pnpm-lock.yaml', 'pnpm-workspace.yaml', '.npmrc', '.pnpmfile.cjs', 'pnpmfile.cjs',
    }) or any(path.startswith('tsconfig') and path.endswith('.json') for path in files)
    packages = any(path.startswith('packages/') for path in files)
    examples = any(path.startswith('examples/') for path in files)
    js_source = {path for path in files if PurePosixPath(path).suffix in JS_SOURCE}
    js_scripts = any(path.startswith('scripts/') for path in js_source)
    js = js_inputs or packages or examples or js_scripts
    js_lint = js or any(path.startswith('apps/') for path in js_source) or any(
        path == 'oxlint.config.ts' for path in files
    )
    format_check = bool(files & {'.editorconfig', '.prettierignore'}) or any(
        PurePosixPath(path).suffix.lower() in FORMAT_TYPES or path.startswith(('.prettierrc', 'prettier.config.'))
        for path in paths
    )
    scans = {
        'actions': any(path.startswith(('.github/workflows/', '.github/actions/')) for path in files),
        'javascript-typescript': js_inputs or bool(js_source),
        'rust': rust_inputs,
    }
    return {
        'lint': full or js_lint or format_check,
        'js-lint': full or js_lint,
        'rust': full or rust_inputs or bool(files & RUST_CHECK_CONFIGS),
        'versions': full or 'Cargo.toml' in files,
        'js': full or js,
        'js-coverage': full or js_inputs or packages or js_scripts or bool(
            files & {'.github/workflows/coverage.yml', 'codecov.yml'}
        ),
        'e2e': full or rust_inputs or js,
        'codeql': [language for language, affected in scans.items() if full or affected],
    }


def main():
    full = os.environ['CI_FULL'] == 'true'
    scope = classify([] if full else changed_paths(), full=full)
    with Path(os.environ['GITHUB_OUTPUT']).open('a') as output:
        for job, enabled in scope.items():
            print(f'{job}={json.dumps(enabled, separators=(",", ":"))}', file=output)


if __name__ == '__main__':
    main()
