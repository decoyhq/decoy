"""Post a fresh comment using trusted run metadata and an untrusted text report."""

import json
import os
import subprocess
from io import BytesIO
from pathlib import Path
from unicodedata import category
from urllib.parse import urlencode
from zipfile import ZipFile

MAX_REPORT = 24_000


def artifact_for(artifacts, attempt):
    matches = [item for item in artifacts if item['name'] == f'ub-mode-report-{attempt}' and not item['expired']]
    return matches[0] if len(matches) == 1 else None


def pr_number(run, repo_id, pulls, *, linked=False):
    matches = {
        pr['number'] for pr in pulls
        if (pr['head'].get('repo') or {}).get('id') == run['head_repository']['id']
        and pr['head']['ref'] == run['head_branch']
        and (linked or pr['head']['sha'] == run['head_sha'])
        and pr['base']['repo']['id'] == repo_id
    }
    return matches.pop() if len(matches) == 1 else None


def read_report(data):
    if len(data) > MAX_REPORT * 2:
        raise ValueError('Report archive is too large')
    with ZipFile(BytesIO(data)) as archive:
        entries = archive.infolist()
        if len(entries) != 1 or entries[0].filename != 'ub-mode.txt' or entries[0].file_size > MAX_REPORT:
            raise ValueError('Unexpected report archive contents')
        return archive.read(entries[0]).decode('utf-8')


def comment_body(run_url, sha, report):
    # No mentions, links, Markdown directives, or terminal controls from PR data.
    text = ''.join(c for c in report if c in '\n\t' or not category(c).startswith('C'))
    text = text.replace('`', 'ˋ').replace('@', '＠').strip()
    return f'### UB-mode report (advisory)\n\nPR commit `{sha}` · [CI attempt]({run_url})\n\n```text\n{text}\n```'


def api(endpoint, *options, data=None):
    return subprocess.check_output(['gh', 'api', *options, endpoint], input=data, timeout=30)


def main():
    event = json.loads(Path(os.environ['GITHUB_EVENT_PATH']).read_text())
    run = event['workflow_run']
    repository = event['repository']
    if run['event'] != 'pull_request' or run['path'] != '.github/workflows/ci.yml' or not run.get('head_repository'):
        return
    repo = f'repos/{repository["full_name"]}'
    attempt = run['run_attempt']
    query = urlencode({'name': f'ub-mode-report-{attempt}'})
    artifacts = json.loads(api(f'{repo}/actions/runs/{run["id"]}/artifacts?{query}'))['artifacts']
    artifact = artifact_for(artifacts, attempt)
    if artifact is None:
        return
    number = pr_number(run, repository['id'], run['pull_requests'], linked=True)
    if not number and not run['pull_requests']:
        # GitHub omits linked PR metadata for runs from forks.
        owner = run['head_repository']['owner']['login']
        query = urlencode({'state': 'all', 'head': f'{owner}:{run["head_branch"]}', 'per_page': 100})
        pages = json.loads(api(f'{repo}/pulls?{query}', '--paginate', '--slurp'))
        number = pr_number(run, repository['id'], [pr for page in pages for pr in page])
        if number is None:
            # A newer commit may have advanced the fork while CI was running.
            pages = json.loads(api(f'{repo}/commits/{run["head_sha"]}/pulls', '--paginate', '--slurp'))
            number = pr_number(run, repository['id'], [pr for page in pages for pr in page], linked=True)
    if number is None:
        raise ValueError('Cannot uniquely bind the report run to a PR')
    if artifact['size_in_bytes'] > MAX_REPORT * 2:
        raise ValueError('Report artifact is too large')
    report = read_report(api(f'{repo}/actions/artifacts/{artifact["id"]}/zip'))
    run_url = f'{os.environ["GITHUB_SERVER_URL"]}/{repository["full_name"]}/actions/runs/{run["id"]}/attempts/{attempt}'
    body = comment_body(run_url, run['head_sha'], report)
    api(f'{repo}/issues/{number}/comments', '--method', 'POST', '--input', '-', data=json.dumps({'body': body}).encode())
    print(f'Published UB-mode report on PR #{number}.')


if __name__ == '__main__':
    main()
