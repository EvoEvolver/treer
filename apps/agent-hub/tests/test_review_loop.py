"""flows/review_loop.py against fake hubs over HTTP (the hubs' public API only)."""
import contextlib
import importlib.util
import io
import json
import os
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location('review_loop', HERE.parent / 'flows' / 'review_loop.py')
loop = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(loop)


def impl_outcome(branch, commit, status='done'):
    return {'front': {'status': status, 'summary': f'Implemented on {branch}.', 'branch': branch, 'commits': commit},
            'refs': {'branch': branch, 'commits': [commit]}}


def review_outcome(verdict, notes='none', status='done', front_verdict=True):
    front = {'status': status, 'summary': f'Review: {verdict}.'}
    if front_verdict:
        front['verdict'] = verdict
    body = (f'## What changed\nnothing (review only)\n\n## Findings\n- major: x\n\n## Verification\nran it\n\n'
            f'## Open questions\nnone\n\n## Next steps\nnone\n\n## Notes for the implementer\n{notes}\n')
    return {'front': front, 'refs': {'branch': None, 'commits': []}, 'body': body}


class FakeHub:
    """POST /api/jobs takes the next scripted outcome; GET /api/jobs/<id> reports it after `polls` reads."""

    def __init__(self, address, outcomes):
        self.address, self.outcomes, self.jobs, self.created = address, list(outcomes), {}, []
        hub = self

        class Handler(BaseHTTPRequestHandler):
            def reply(self, status, value):
                raw = json.dumps(value).encode()
                self.send_response(status)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

            def do_GET(self):
                parts = self.path.split('?')[0].strip('/').split('/')
                if parts == ['api', 'me']:
                    return self.reply(200, {'address': hub.address})
                job = hub.jobs.get(parts[2]) if len(parts) == 3 else None
                if not job:
                    return self.reply(404, {'error': {'code': 'job_not_found', 'message': 'no such job'}})
                if parts[1] == 'jobs':
                    job['polls'] -= 1
                    done = job['polls'] <= 0
                    status = job['outcome'].get('job_status', 'archived') if done else 'running'
                    return self.reply(200, {'id': job['id'], 'status': status, 'error': job['outcome'].get('error'),
                                            'handoff': {'status': job['outcome']['front']['status']}
                                            if done and 'front' in job['outcome'] else None})
                out = job['outcome']
                front = dict(out['front'], job=job['id'], hub=hub.address, template=job['body']['template'])
                markdown = '---\n' + ''.join(f'{k}: {v}\n' for k, v in front.items()) + '---\n\n' + out.get('body', '')
                self.reply(200, {'job': job['id'], 'ref': f"{hub.address}/{job['id']}", 'markdown': markdown,
                                 'front': front, 'refs': out['refs']})

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                if not hub.outcomes:
                    return self.reply(500, {'error': {'code': 'unexpected', 'message': 'no scripted outcome left'}})
                outcome = hub.outcomes.pop(0)
                if 'refuse' in outcome:
                    return self.reply(400, {'error': {'code': 'input_handoff_unresolved', 'message': outcome['refuse']}})
                job_id = f'j-{len(hub.jobs) + 1:04d}'
                hub.jobs[job_id] = {'id': job_id, 'body': body, 'outcome': outcome, 'polls': 2}
                hub.created.append(body)
                self.reply(201, {'id': job_id, 'status': 'queued', 'template': body['template'] + '@1'})

            def log_message(self, *_):
                pass
        self.http = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=self.http.serve_forever, daemon=True).start()
        self.url = f'http://127.0.0.1:{self.http.server_address[1]}/'

    def close(self):
        self.http.shutdown()
        self.http.server_close()


class ReviewLoopTests(unittest.TestCase):
    def setUp(self):
        self.saved = dict(os.environ)
        os.environ['no_proxy'] = os.environ['NO_PROXY'] = '127.0.0.1,localhost'
        self.hubs = []

    def tearDown(self):
        for h in self.hubs:
            h.close()
        os.environ.clear()
        os.environ.update(self.saved)

    def hub(self, address, outcomes):
        h = FakeHub(address, outcomes)
        self.hubs.append(h)
        return h

    def run_loop(self, impl, review, *extra, task='Add the feature.'):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = loop.main(['--impl-hub', impl.url, '--review-hub', review.url, '--impl-template', 'web-implementer',
                              '--review-template', 'reviewer', '--task', task, '--base-branch', 'origin/main',
                              '--poll-seconds', '0.01', *extra])
        lines = out.getvalue().strip().splitlines()
        return code, json.loads(lines[-1]), out.getvalue(), err.getvalue()

    def test_approve_in_round_two_across_hubs(self):
        notes = '1. `app.py:12` restores the deleted assertion; run `pytest -k foo`.\n2. Keep the README in sync.'
        impl = self.hub('/hub_linux', [impl_outcome('agent/linux-j-0001', 'aaaaaaa1'),
                                       impl_outcome('agent/linux-j-0002', 'bbbbbbb2')])
        review = self.hub('/hub_mac', [review_outcome('changes-needed', notes), review_outcome('approve')])
        with tempfile.NamedTemporaryFile('w', suffix='.md', delete=False) as f:
            f.write('Add the feature.\n')
        self.addCleanup(os.unlink, f.name)
        code, result, out, err = self.run_loop(impl, review, task='@' + f.name)
        self.assertEqual(code, 0, out + err)
        self.assertEqual((result['outcome'], result['rounds'], result['verdict'], result['branch'], result['commit']),
                         ('approved', 2, 'approve', 'agent/linux-j-0002', 'bbbbbbb2'))
        self.assertEqual((result['implementation'], result['review']), ('/hub_linux/j-0002', '/hub_mac/j-0002'))
        self.assertIn('approved after 2 round(s); final branch agent/linux-j-0002 at bbbbbbb2, verdict approve', out)
        first, second = impl.created
        self.assertEqual((first['template'], first['task'], first['inputs']),
                         ('web-implementer', 'Add the feature.\n', {'branch': 'origin/main', 'handoffs': []}))
        # Round 2: the task, the reviewer's notes verbatim, and both handoffs by address.
        self.assertTrue(second['task'].startswith('Add the feature.\n\n## Review loop: round 2 of at most 4'))
        self.assertIn(notes, second['task'])
        self.assertIn('Start from branch `agent/linux-j-0001`', second['task'])
        self.assertEqual(second['inputs'], {'branch': 'agent/linux-j-0001', 'handoffs': ['/hub_mac/j-0001', 'j-0001']})
        r1, r2 = review.created
        self.assertEqual(r1['inputs'], {'branch': 'agent/linux-j-0001', 'handoffs': ['/hub_linux/j-0001']})
        self.assertEqual(r2['inputs'], {'branch': 'agent/linux-j-0002', 'handoffs': ['/hub_linux/j-0002', 'j-0001']})
        self.assertIn("implementer's own account of the work, not evidence", r1['task'])
        self.assertIn('weakened or special-cased tests', r1['task'])
        self.assertIn('/hub_mac/j-0001 is the previous round', r2['task'].replace('Input handoff ', ''))
        self.assertIn('## The task the implementer was given\n\nAdd the feature.', r1['task'])

    def test_one_hub_uses_local_references(self):
        same = self.hub('/agent_hub', [impl_outcome('feat/a', 'aaaaaaa1'), review_outcome('changes-needed', 'fix it'),
                                       impl_outcome('feat/b', 'bbbbbbb2'), review_outcome('approve')])
        code, result, _, _ = self.run_loop(same, same)
        self.assertEqual(code, 0)
        self.assertEqual([b['inputs']['handoffs'] for b in same.created], [[], ['j-0001'], ['j-0002', 'j-0001'], ['j-0003', 'j-0002']])

    def test_max_rounds_exits_2_with_the_open_notes(self):
        impl = self.hub('/a', [impl_outcome('b1', 'aaaaaaa1'), impl_outcome('b2', 'bbbbbbb2')])
        review = self.hub('/b', [review_outcome('changes-needed', 'n1'), review_outcome('changes-needed', 'n2 still open')])
        code, result, out, _ = self.run_loop(impl, review, '--max-rounds', '2')
        self.assertEqual(code, 2)
        self.assertEqual((result['outcome'], result['rounds'], result['verdict'], result['open_notes']),
                         ('max-rounds', 2, 'changes-needed', 'n2 still open'))
        self.assertEqual((len(impl.created), len(review.created)), (2, 2))

    def test_errors_exit_1(self):
        cases = [
            ('failed implementer', [dict(impl_outcome('b', 'a1b2c3d'), job_status='failed', error='worker stalled')], [],
             'ended failed without a usable handoff: worker stalled'),
            ('blocked implementer', [impl_outcome('b', 'a1b2c3d', status='blocked')], [], 'is blocked'),
            ('no branch', [{'front': {'status': 'done', 'summary': 's'}, 'refs': {'branch': None, 'commits': []}}], [],
             'names no branch'),
            ('refused', [impl_outcome('b', 'a1b2c3d')], [{'refuse': 'input handoff /a/j-0001: no App /a'}],
             'HTTP 400 input_handoff_unresolved: input handoff /a/j-0001: no App /a'),
            ('no verdict', [impl_outcome('b', 'a1b2c3d')], [review_outcome('approve', front_verdict=False)],
             'no machine-readable verdict'),
            ('blocked reviewer', [impl_outcome('b', 'a1b2c3d')], [review_outcome('approve', status='blocked')], 'is blocked'),
        ]
        for name, impl_out, review_out, why in cases:
            with self.subTest(name):
                impl, review = self.hub('/a', impl_out), self.hub('/b', review_out)
                code, result, _, _ = self.run_loop(impl, review)
                self.assertEqual((code, result['outcome']), (1, 'error'))
                self.assertIn(why, result['error'])
        code, result, _, _ = self.run_loop(self.hub('/x', []), self.hubs[0], task='@/nonexistent/task.md')
        self.assertIn('--task:', result['error'])
        dead = FakeHub('/dead', [])
        dead.close()
        code, result, _, _ = self.run_loop(dead, dead)
        self.assertEqual(code, 1)
        self.assertIn('POST ' + dead.url + 'api/jobs: ', result['error'])

    def test_verdict_parsing(self):
        markdown = ('---\njob: j-0002\nverdict: changes-needed   # approve | changes-needed\n---\n\n## Findings\n- major: a\n\n'
                    '## Notes for the implementer\n1. Fix `a.py:3`.\n\n### Detail\nkeep this\n\n## Other\ndrop this\n')
        self.assertEqual(loop.parse_verdict({'markdown': markdown}),
                         ('changes-needed', '1. Fix `a.py:3`.\n\n### Detail\nkeep this'))
        self.assertEqual(loop.parse_verdict({'markdown': markdown, 'front': {'verdict': 'approve'}})[0], 'approve',
                         'the hub-parsed front matter wins')
        self.assertEqual(loop.parse_verdict({'markdown': '---\nverdict: Approve\n---\n\n## Findings\nnone\n'}), ('approve', 'none'))
        self.assertEqual(loop.parse_verdict({'markdown': '---\nverdict: approve\n---\n\nAll good.\n'}), ('approve', 'All good.'))
        for bad in ('---\nstatus: done\n---\n', '---\nverdict: lgtm\n---\n', 'no front matter'):
            with self.assertRaises(loop.LoopError):
                loop.parse_verdict({'markdown': bad, 'ref': '/b/j-0001'})


if __name__ == '__main__':
    unittest.main()
