from pathlib import Path
from tempfile import TemporaryDirectory
from unittest import TestCase

from ub_mode_report import changed_lines, crap_findings, duplicate_findings, source_path


class ReportTest(TestCase):
    def test_source_scope(self):
        for path in ('crates/decoy/src/main.rs', 'packages/cli/src/index.ts', 'scripts/build.mjs'):
            self.assertTrue(source_path(path), path)
        for path in ('README.md', 'crates/decoy/Cargo.toml', 'packages/cli/rstest.config.ts', 'packages/cli/src/main.test.ts'):
            self.assertFalse(source_path(path), path)

    def test_changed_lines_include_deletion_inside_function(self):
        self.assertEqual(changed_lines('a\nb\nc\n', 'a\nc\n'), {2})
        self.assertEqual(changed_lines('a\nb\n', 'a\nb\n'), set())

    def test_score_threshold_and_missing_coverage(self):
        path = 'packages/cli/src/index.ts'
        source = 'function run(x: number) {\n  if (x > 0) return 1;\n  if (x < 0) return 2;\n  return 0;\n}\n'
        with TemporaryDirectory() as tmp:
            report = Path(tmp) / 'lcov.info'
            report.write_text(f'SF:{path}\nDA:2,0\nend_of_record\n')
            self.assertIn('CRAP 12.0', crap_findings({path: source}, {path: {2}}, [report])[0])
            self.assertEqual(crap_findings({path: source}, {path: {2}}, [],), [f'{path}:1 run — complexity 3, CRAP unavailable'])
            self.assertEqual(crap_findings({path: source}, {path: set()}, [report]), [])

    def test_new_duplicate_against_untouched_file_but_not_existing_pair(self):
        left = 'packages/cli/src/left.ts'
        right = 'packages/cli/src/right.ts'
        source = ('function run(x: number) {\n  if (x > 0) return Math.max(x, 10);\n'
                  '  if (x < 0) return Math.min(x, 10);\n'
                  '  const arr = Array.from({ length: x }, (_, i) => Math.abs(i));\n'
                  '  for (const value of arr) { if (value === x) break; }\n'
                  '  const n = arr.filter(Boolean).map(Math.sign).reduce((a,b) => a+b,0);\n'
                  '  try { return Math.abs(x) + n } catch { return -1; }\n}\n')
        finding = {left: source, right: source}
        self.assertEqual(len(duplicate_findings({right: source}, finding, {left: {2}})), 1)
        self.assertEqual(duplicate_findings(finding, finding, {left: {2}}), [])
        # Changing a call changes the fingerprint, but this pair already cleared 80%.
        changed = source.replace('Math.max', 'Math.round')
        self.assertEqual(len(duplicate_findings({right: source}, {left: changed, right: source}, {left: {2}})), 1)
        self.assertEqual(duplicate_findings(finding, {left: changed, right: source}, {left: {2}}), [])
