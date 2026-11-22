import json
from io import BytesIO
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest import TestCase
from unittest.mock import patch
from zipfile import ZipFile

from ub_mode_comment import artifact_for, comment_body, main, pr_number, read_report


class CommentTest(TestCase):
    def test_only_current_attempt(self):
        artifacts = [{'name': 'ub-mode-report-1', 'id': 1, 'expired': False},
                     {'name': 'ub-mode-report-2', 'id': 2, 'expired': False}]
        self.assertEqual(artifact_for(artifacts, 2)['id'], 2)
        self.assertIsNone(artifact_for(artifacts, 3))
        self.assertIsNone(artifact_for([*artifacts, artifacts[1]], 2))

    def test_bind_pr_to_fork_branch_commit_and_base_repository(self):
        run = {'head_repository': {'id': 12}, 'head_branch': 'topic', 'head_sha': 'abc'}
        pr = {'number': 42, 'head': {'repo': {'id': 12}, 'ref': 'topic', 'sha': 'abc'},
              'base': {'repo': {'id': 7}}}
        self.assertEqual(pr_number(run, 7, [pr]), 42)
        advanced = pr | {'head': pr['head'] | {'sha': 'new'}}
        self.assertIsNone(pr_number(run, 7, [advanced]))
        self.assertEqual(pr_number(run, 7, [advanced], linked=True), 42)
        self.assertIsNone(pr_number(run, 8, [pr]))
        self.assertIsNone(pr_number(run, 7, [pr, pr | {'number': 43}]))

    def test_treat_report_as_text(self):
        archive = BytesIO()
        with ZipFile(archive, 'w') as zipped:
            zipped.writestr('ub-mode.txt', '```\n@decoyhq/team\n<img>\n')
        text = read_report(archive.getvalue())
        comment = comment_body('https://github.com/decoyhq/decoy/actions/runs/7/attempts/2', 'abc', text, 2)
        self.assertEqual(comment.count('```'), 2)
        self.assertNotIn('@decoyhq', comment)
        self.assertIn('ˋˋˋ\n＠decoyhq/team\n<img>', comment)
        self.assertIn('/attempts/2', comment)
        self.assertIn('abc', comment)

    def test_reruns_post_fresh_comments_and_no_artifact_skips(self):
        pr = {'number': 42, 'head': {'repo': {'id': 12}, 'ref': 'topic', 'sha': 'abc'},
              'base': {'repo': {'id': 7}}}
        run = {'id': 90, 'event': 'pull_request', 'path': '.github/workflows/ci.yml',
               'head_repository': {'id': 12}, 'head_branch': 'topic', 'head_sha': 'abc', 'pull_requests': [pr]}
        archive = BytesIO()
        with ZipFile(archive, 'w') as zipped:
            zipped.writestr('ub-mode.txt', 'No findings.')
        artifacts = [{'name': f'ub-mode-report-{attempt}', 'id': attempt, 'expired': False,
                      'size_in_bytes': len(archive.getvalue())} for attempt in (1, 2, 4)]
        posts = []

        def fake_api(endpoint, *options, data=None):
            if '/artifacts?' in endpoint:
                return json.dumps({'artifacts': artifacts}).encode()
            if endpoint.endswith('/zip'):
                return archive.getvalue()
            self.assertEqual(endpoint, 'repos/decoyhq/decoy/issues/42/comments')
            if options == ('--paginate', '--slurp'):
                return json.dumps([[{'body': body, 'user': {'login': 'github-actions[bot]'}} for body in posts]]).encode()
            self.assertEqual(options, ('--method', 'POST', '--input', '-'))
            posts.append(json.loads(data)['body'])
            return b'{}'

        with TemporaryDirectory() as tmp:
            event_path = Path(tmp) / 'event.json'
            with patch.dict('os.environ', {'GITHUB_EVENT_PATH': str(event_path), 'GITHUB_SERVER_URL': 'https://github.com'}), patch('ub_mode_comment.api', side_effect=fake_api):
                for attempt, publisher_attempt in ((1, 1), (2, 1), (1, 2), (3, 1), (2, 2), (4, 2)):
                    event_path.write_text(json.dumps({'workflow_run': run | {'run_attempt': attempt},
                                                     'repository': {'id': 7, 'full_name': 'decoyhq/decoy'}}))
                    with patch.dict('os.environ', {'GITHUB_RUN_ATTEMPT': str(publisher_attempt)}):
                        main()
        self.assertEqual(len(posts), 3)
        for attempt, body in zip((1, 2, 4), posts):
            self.assertIn(f'/attempts/{attempt}', body)
            self.assertIn('No findings.', body)

    def test_reject_unexpected_zip_member(self):
        archive = BytesIO()
        with ZipFile(archive, 'w') as zipped:
            zipped.writestr('../ub-mode.txt', 'report')
        with self.assertRaises(ValueError):
            read_report(archive.getvalue())
