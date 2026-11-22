"""Advisory UB-mode metrics for the source diff on the tested merge commit."""

import argparse
import subprocess
from difflib import SequenceMatcher
from functools import lru_cache
from pathlib import Path, PurePosixPath


def source_path(filename):
    path = PurePosixPath(filename)
    if len(path.parts) < 2 or path.parts[0] not in {'crates', 'packages', 'examples', 'scripts'}:
        return False
    if {'test', 'tests', 'spec', 'specs', '__tests__', 'fixtures', 'testdata'} & set(path.parts):
        return False
    if path.name.startswith('.') or any(part in path.name.split('.')[:-1] for part in ('config', 'conf', 'test', 'spec', 'd')):
        return False
    return path.suffix in {'.rs', '.ts', '.tsx', '.mts', '.cts', '.js', '.jsx', '.mjs', '.cjs'}


def git(*args):
    return subprocess.check_output(['git', *args])


def changed_paths(base, head):
    return {path for path in git('diff', '--name-only', '-z', '--no-renames', base, head).decode().split('\0') if source_path(path)}


@lru_cache(maxsize=None)
def blob(oid):
    return git('cat-file', 'blob', oid).decode(errors='replace')


def sources(rev):
    result = {}
    for entry in git('ls-tree', '-r', '-z', rev).decode().split('\0'):
        if entry:
            metadata, path = entry.split('\t', 1)
            mode, _, oid = metadata.split()
            if mode in {'100644', '100755'} and source_path(path):
                result[path] = blob(oid)
    return result


def changed_lines(before, after):
    left, right = before.splitlines(), after.splitlines()
    changes = set()
    for tag, _, _, start, end in SequenceMatcher(None, left, right, autojunk=False).get_opcodes():
        if tag == 'delete':
            # A deletion changes the gap, not either neighboring line.
            changes.add((start, start + 1))
        elif tag != 'equal':
            changes.update((line, line) for line in range(start + 1, end + 1))
    return changes


def touches(changes, path, start, end):
    return any(start <= first and last <= end for first, last in changes.get(path, ()))


def crap_findings(source, changes, reports):
    from crapper.coverage import CoverageBundle, parse_lcov
    from crapper.crap import crap_score
    from crapper.discover import language_of
    from crapper.languages import functions_in_file

    bundle = CoverageBundle()
    for report in reports:
        for filename, record in parse_lcov(report.read_text(errors='replace')).items():
            # rstest can report paths relative to the package, not the repository.
            if filename.startswith('src/') and report.name == 'lcov.info' and report.parent.parent.parent.name == 'packages':
                filename = f'packages/{report.parent.parent.name}/{filename}'
            bundle.lcov[filename] = record
    bundle.bind_sources(list(source))
    findings = []
    for path in sorted(changes.keys() & source.keys()):
        for function in functions_in_file(language_of(path), source[path], path, str(Path.cwd())):
            if not touches(changes, path, function.start_line, function.end_line):
                continue
            score = crap_score(function.complexity, bundle.percent_for(function))
            label = f'{path}:{function.start_line} {function.name}'
            if score is None:
                findings.append((0, f'{label} — complexity {function.complexity}, CRAP unavailable'))
            elif score >= 10:
                findings.append((score, f'{label} — CRAP {score:.1f}'))
    return [text for _, text in sorted(findings, key=lambda finding: (-finding[0], finding[1]))]


def previous_forms(before, after, old, new):
    """Match forms through stable lines; also recognize identical moved forms."""
    blocks = SequenceMatcher(None, before.splitlines(), after.splitlines(), autojunk=False).get_matching_blocks()
    matches = []
    for i, current in enumerate(new):
        for j, previous in enumerate(old):
            overlap = sum(max(0, min(left + length, previous.end_line, current.end_line - right + left)
                                 - max(left + 1, previous.start_line, current.start_line - right + left) + 1)
                          for left, right, length in blocks)
            if overlap or current.fingerprints == previous.fingerprints:
                matches.append((overlap, i, j))
    result, used = {}, set()
    for _, i, j in sorted(matches, key=lambda match: -match[0]):
        if i not in result and j not in used:
            result[i] = old[j]
            used.add(j)
    return result


def duplicate_findings(base, head, changes):
    from dryer.discover import language_of
    from dryer.extract import entries_in_source
    from dryer.shape import jaccard

    def forms(path, text):
        suffix = PurePosixPath(path).suffix
        language = 'typescript' if suffix in {'.js', '.jsx', '.mjs', '.cjs'} else language_of(path)
        grammar_path = f'{path}.tsx' if suffix == '.jsx' else path
        entries, warning = entries_in_source(language, text, grammar_path, path)
        if warning:
            raise ValueError(f'{path}: {warning}')
        return [entry for entry in entries if entry.nodes >= 20 and entry.end_line - entry.start_line >= 3]

    entries = []
    for path, text in sorted(head.items()):
        current = forms(path, text)
        if path in changes:
            previous = previous_forms(base[path], text, forms(path, base[path]), current) if path in base else {}
        else:
            previous = dict(enumerate(current))
        for index, entry in enumerate(current):
            entries.append((entry, previous.get(index), touches(changes, path, entry.start_line, entry.end_line)))

    findings = []
    for index, (left, old_left, touched) in enumerate(entries):
        if not touched:
            continue
        for other, (right, old_right, right_touched) in enumerate(entries):
            if index == other or left.language != right.language or (other < index and right_touched):
                continue
            score = jaccard(left.fingerprints, right.fingerprints)
            if score < 0.8 or (old_left and old_right and jaccard(old_left.fingerprints, old_right.fingerprints) >= 0.8):
                continue
            findings.append((score, f'{left.file}:{left.start_line} ↔ {right.file}:{right.start_line} — {score:.0%}'))
    return [text for _, text in sorted(findings, key=lambda finding: (-finding[0], finding[1]))]


def section(title, findings):
    rows = [row.encode()[:500].decode(errors='ignore') for row in findings[:15]] or ['None.']
    if len(findings) > 15:
        rows.append(f'… and {len(findings) - 15} more.')
    return '\n'.join([title, *rows])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check-paths', action='store_true')
    parser.add_argument('--coverage', type=Path)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    base, head = 'HEAD^1', 'HEAD'
    paths = changed_paths(base, head)
    if args.check_paths:
        print('true' if paths else 'false')
        return
    if not paths:
        return
    before, after = sources(base), sources(head)
    changes = {path: changed_lines(before.get(path, ''), after.get(path, '')) for path in paths}
    reports = sorted(path for path in args.coverage.rglob('*') if path.is_file() and path.name in {'rust.lcov', 'lcov.info'})
    results = []
    for title, scan in (
        ('Crapper — CRAP ≥ 10', lambda: crap_findings(after, changes, reports)),
        ('Dryer — new structural matches ≥ 80%', lambda: duplicate_findings(before, after, changes)),
    ):
        try:
            results.append(section(title, scan()))
        except Exception as exc:
            print(f'{title}: {exc!r}')
            results.append(section(title, ['Analysis unavailable. See the job log.']))
    args.output.write_text('\n\n'.join(results) + '\n')


if __name__ == '__main__':
    main()
