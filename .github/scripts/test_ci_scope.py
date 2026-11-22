from unittest import TestCase
from unittest.mock import patch

from ci_scope import changed_paths, classify


class ScopeTest(TestCase):
    def assert_scope(self, files, *, jobs=(), codeql=(), full=False):
        result = classify(files, full=full)
        flags = {key: enabled for key, enabled in result.items() if key != 'codeql'}
        self.assertTrue(set(jobs) <= flags.keys(), f'Missing flags: {set(jobs) - flags.keys()}')
        for key, enabled in flags.items():
            self.assertIs(enabled, key in jobs, key)
        self.assertEqual(result['codeql'], list(codeql))

    def test_docs_even_under_source_roots(self):
        self.assert_scope(['docs/guide/http.md', 'crates/decoy-runtime/AGENTS.md', 'packages/cli/README.md'], jobs=['lint'])

    def test_unrelated_automation_does_not_run_product_jobs(self):
        self.assert_scope(['.github/scripts/ub_mode_report.py'])
        self.assert_scope(['.github/workflows/ci.yml', '.github/workflows/codeql.yml'], jobs=['lint'], codeql=['actions'])
        self.assert_scope(['.github/workflows/coverage.yml'], jobs=['lint', 'js-coverage'], codeql=['actions'])

    def test_rust_and_js_are_independent(self):
        self.assert_scope(['crates/decoy/src/main.rs'], jobs=['rust', 'e2e'], codeql=['rust'])
        self.assert_scope(['packages/cli/src/view.tsx'], jobs=['lint', 'js-lint', 'js', 'js-coverage', 'e2e'], codeql=['javascript-typescript'])
        self.assert_scope(['examples/ws-node/index.jsx'], jobs=['lint', 'js-lint', 'js', 'e2e'], codeql=['javascript-typescript'])

    def test_dependencies_and_toolchains_still_select_affected_jobs(self):
        self.assert_scope(['Cargo.lock'], jobs=['rust', 'e2e'], codeql=['rust'])
        self.assert_scope(['pnpm-lock.yaml'], jobs=['lint', 'js-lint', 'js', 'js-coverage', 'e2e'], codeql=['javascript-typescript'])
        for hook in ('.pnpmfile.cjs', 'pnpmfile.cjs'):
            self.assert_scope([hook], jobs=['lint', 'js-lint', 'js', 'js-coverage', 'e2e'], codeql=['javascript-typescript'])
        self.assert_scope(['Cargo.toml'], jobs=['versions', 'rust', 'e2e'], codeql=['rust'])
        self.assert_scope(['.prototools'], jobs=['lint', 'js-lint', 'rust', 'js', 'js-coverage', 'e2e'], codeql=['javascript-typescript', 'rust'])
        self.assert_scope(['.npmrc'], jobs=['lint', 'js-lint', 'js', 'js-coverage', 'e2e'], codeql=['javascript-typescript'])
        self.assert_scope(['.cargo/config.toml'], jobs=['rust', 'e2e'], codeql=['rust'])

    def test_configs_only_run_their_consumers(self):
        self.assert_scope(['eslint.config.ts'], jobs=['lint', 'js-lint'], codeql=['javascript-typescript'])
        self.assert_scope(['.prettierignore'], jobs=['lint'])
        self.assert_scope(['.coderabbit.yaml'], jobs=['lint'])
        self.assert_scope(['deny.toml'], jobs=['rust'])
        for config in ('rustfmt.toml', '.rustfmt.toml', 'clippy.toml', '.clippy.toml'):
            self.assert_scope([config], jobs=['rust'])

    def test_full_mode_and_no_diff(self):
        self.assert_scope([], jobs=[])
        self.assert_scope([], full=True, jobs=['lint', 'js-lint', 'js', 'js-coverage', 'rust', 'e2e', 'versions'], codeql=['actions', 'javascript-typescript', 'rust'])

    @patch('ci_scope.subprocess.check_output', return_value=b'crates/old.rs\x00docs/file with space.md\x00')
    def test_read_tested_diff_including_removed_paths(self, git):
        self.assertEqual(changed_paths(), ['crates/old.rs', 'docs/file with space.md'])
        git.assert_called_once_with(['git', 'diff', '--name-only', '-z', '--no-renames', 'HEAD^1', 'HEAD'])
