#!/usr/bin/env python3
"""Review loop over Agent Hubs: implement, review fresh, pass the notes on, repeat (stdlib only).

A caller-side flow that only uses the hubs' public HTTP API; the hub itself stays a
one-job primitive. Each round:

1. A fresh implementer job (round 1: the task; later rounds: the task, the previous
   reviewer's notes verbatim and the previous implementation and review handoffs as
   input references) on --impl-hub. Wait until it is archived; read its handoff
   (status, branch, commits).
2. A fresh reviewer job on --review-hub with the implementation handoff (and the
   previous review) as input references. The implementer's handoff is its own
   account, not evidence: the reviewer holds every claim to the repository.
3. Read the reviewer's `verdict` (front matter) and `## Notes for the implementer`.
   `approve` ends the loop; `changes-needed` starts the next round.

Exit status: 0 approved, 2 --max-rounds reached without approval, 1 error (a job
failed or was stopped, a handoff is blocked or unreadable, a hub refused a request).
Progress goes to stderr; the summary (text, then one JSON line) to stdout.

Run it as an Agent (its runtime proxy carries the identity for http://SERVICE.treer/):

    python3 review_loop.py --impl-hub http://svc_A.treer/ --review-hub http://svc_B.treer/ \\
      --impl-template implementer --review-template reviewer --base-branch origin/main --task @task.md
"""
import argparse
import json
import re
import sys
import time
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

VERDICTS = ('approve', 'changes-needed')
NOTES = 'Notes for the implementer'
FINAL = ('archived', 'failed', 'stopped')


class LoopError(Exception):
    pass


def log(*parts):
    print('[review-loop]', *parts, file=sys.stderr, flush=True)


def front_matter(markdown):
    """`key: value` lines between the leading `---` fences."""
    match = re.match(r'---\n(.*?)\n---', (markdown or '').replace('\r\n', '\n'), re.S)
    front = {}
    for line in (match.group(1).splitlines() if match else []):
        name, sep, value = line.partition(':')
        if sep and re.fullmatch(r'[a-z_]{1,32}', name.strip()):
            front[name.strip()] = value.split(' #')[0].strip().strip('"\'')
    return front


def section(markdown, title):
    """The text under a `## title` heading up to the next heading of the same or a higher level."""
    match = re.search(r'^(#{1,3})\s+' + re.escape(title) + r'\b[^\n]*\n', markdown or '', re.M | re.I)
    if not match:
        return None
    rest = markdown[match.end():]
    end = re.search(r'^#{1,%d}\s' % len(match.group(1)), rest, re.M)
    return (rest[:end.start()] if end else rest).strip()


def parse_verdict(handoff):
    """(verdict, notes) from a reviewer handoff (the hub's JSON handoff view)."""
    markdown = handoff.get('markdown') or ''
    front = dict(front_matter(markdown), **(handoff.get('front') or {}))
    verdict = (front.get('verdict') or '').split(' #')[0].strip().lower()
    if verdict not in VERDICTS:
        raise LoopError(f"review handoff {handoff.get('ref') or handoff.get('job')} has no machine-readable verdict "
                        f"(front matter `verdict: approve|changes-needed`; got {front.get('verdict')!r})")
    notes = section(markdown, NOTES)
    if notes is None:
        notes = section(markdown, 'Findings')
    if notes is None:
        # Not a hub-validated reviewer handoff: pass the whole report on rather than nothing.
        notes = re.sub(r'\A---\n.*?\n---\n', '', markdown.replace('\r\n', '\n'), flags=re.S).strip()
    return verdict, notes


class Hub:
    """One Agent Hub's public HTTP API."""

    def __init__(self, url, timeout=60):
        self.url = url.rstrip('/') + '/'
        self.timeout = timeout
        self._address = None

    def call(self, method, path, body=None):
        data = None if body is None else json.dumps(body).encode()
        request = Request(self.url + path, data=data, method=method,
                          headers={'Accept': 'application/json', **({'Content-Type': 'application/json'} if data else {})})
        try:
            with urlopen(request, timeout=self.timeout) as response:
                return json.loads(response.read() or b'{}')
        except HTTPError as error:
            detail = error.read(65536).decode('utf-8', 'replace')
            try:
                detail = '{code}: {message}'.format(**json.loads(detail)['error'])
            except (ValueError, KeyError, TypeError):
                detail = detail[:300]
            raise LoopError(f'{method} {self.url}{path}: HTTP {error.code} {detail}') from None
        except (URLError, OSError, ValueError) as error:
            raise LoopError(f'{method} {self.url}{path}: {error}') from None

    @property
    def address(self):
        if self._address is None:
            self._address = self.call('GET', 'api/me')['address']
        return self._address

    def ref(self, job_id, for_hub):
        """How `for_hub` names this hub's handoff `job_id` in inputs.handoffs."""
        return job_id if self.address == for_hub.address else f'{self.address}/{job_id}'

    def create(self, template, task, inputs):
        job = self.call('POST', 'api/jobs', {'template': template, 'task': task, 'inputs': inputs})
        log(f"created {self.address}/jobs/{job['id']} ({job['template']})")
        return job

    def wait(self, job_id, poll, timeout):
        deadline = time.monotonic() + timeout if timeout else None
        while True:
            job = self.call('GET', f'api/jobs/{job_id}')
            if job['status'] in FINAL:
                return job
            if deadline and time.monotonic() > deadline:
                raise LoopError(f'{self.address}/jobs/{job_id} still {job["status"]} after {timeout}s '
                                f'(it keeps running; stop it with POST {self.url}api/jobs/{job_id}/stop)')
            time.sleep(poll)

    def finished_handoff(self, job_id, poll, timeout):
        job = self.wait(job_id, poll, timeout)
        if job['status'] != 'archived' or not job.get('handoff'):
            raise LoopError(f"{self.address}/jobs/{job_id} ended {job['status']} without a usable handoff: {job.get('error')}")
        handoff = self.call('GET', f'api/handoffs/{job_id}')
        handoff.setdefault('ref', f'{self.address}/{job_id}')
        return handoff


def read_task(value):
    if value.startswith('@'):
        with open(value[1:], encoding='utf-8') as f:
            return f.read()
    return value


def impl_task(task, round_, args, impl, review, notes):
    if round_ == 1:
        return task
    commits = ' '.join(impl['refs'].get('commits') or []) or 'none listed'
    return (f"{task.rstrip()}\n\n"
            f"## Review loop: round {round_} of at most {args.max_rounds}\n\n"
            f"The previous implementation (input handoff {impl['ref']}: branch `{impl['refs']['branch']}`, commits "
            f"{commits}) was reviewed by a fresh reviewer (input handoff {review['ref']}) with verdict "
            f"`changes-needed`. Start from branch `{impl['refs']['branch']}` (fetch it the way the task's branches are "
            f"fetched), not from `{args.base_branch}`, and address every note below; the review is on the actual "
            f"repository state, not on that handoff's own account. Report each note as fixed or, with the reason, "
            f"not fixed. The notes are the reviewer's, passed on verbatim:\n\n{notes.strip()}\n")


def review_task(task, round_, args, impl, previous_review):
    commits = ' '.join(impl['refs'].get('commits') or []) or 'none listed'
    lines = [f"Review loop round {round_} of at most {args.max_rounds}: review branch `{impl['refs']['branch']}` "
             f"(commits {commits}) against base `{args.base_branch}`.", '',
             f"Input handoff {impl['ref']} is the implementer's own account of the work, not evidence: hold every "
             f"claim in it to the repository, run the checks it claims, and treat weakened or special-cased tests and "
             f"faked work as the main thing to catch.",
             f"Its handoff status is `{impl['front'].get('status')}`: {impl['front'].get('summary', '')}"]
    if previous_review:
        lines.append(f"Input handoff {previous_review['ref']} is the previous round's review: check each of its notes "
                     f"(fixed or still open).")
    lines += ['', 'Return `verdict: approve` or `verdict: changes-needed` in the front matter and a '
              '`## Notes for the implementer` section written to be passed verbatim to a fresh implementer.', '',
              '## The task the implementer was given', '', task.strip()]
    return '\n'.join(lines) + '\n'


def run(args, state):
    """The loop; `state` collects rounds and the latest notes so an error still reports them."""
    impl_hub, review_hub = Hub(args.impl_hub), Hub(args.review_hub)
    try:
        task = read_task(args.task)
    except OSError as error:
        raise LoopError(f'--task: {error}') from None
    if not task.strip():
        raise LoopError('--task is empty')
    impl = review = None
    notes = ''
    rounds = state['rounds']
    for round_ in range(1, args.max_rounds + 1):
        inputs = {'branch': args.base_branch if round_ == 1 else impl['refs']['branch'], 'handoffs': []}
        if args.repo:
            inputs['repo'] = args.repo
        if round_ > 1:
            inputs['handoffs'] = [review_hub.ref(review['job'], impl_hub), impl_hub.ref(impl['job'], impl_hub)]
        log(f'round {round_}: implementer')
        job = impl_hub.create(args.impl_template, impl_task(task, round_, args, impl, review, notes), inputs)
        impl = impl_hub.finished_handoff(job['id'], args.poll_seconds, args.job_timeout_seconds)
        status = impl['front'].get('status')
        log(f"round {round_}: implementation {impl['ref']} {status}: {impl['front'].get('summary', '')}")
        if status == 'blocked':
            raise LoopError(f"implementer {impl['ref']} is blocked: {impl['front'].get('summary', '')}")
        if not impl['refs'].get('branch'):
            raise LoopError(f"implementer handoff {impl['ref']} names no branch to review")
        refs = [impl_hub.ref(impl['job'], review_hub)] + ([review_hub.ref(review['job'], review_hub)] if review else [])
        log(f'round {round_}: reviewer')
        job = review_hub.create(args.review_template, review_task(task, round_, args, impl, review),
                                {'branch': impl['refs']['branch'], 'handoffs': refs,
                                 **({'repo': args.repo} if args.repo else {})})
        review = review_hub.finished_handoff(job['id'], args.poll_seconds, args.job_timeout_seconds)
        if review['front'].get('status') == 'blocked':
            raise LoopError(f"reviewer {review['ref']} is blocked: {review['front'].get('summary', '')}")
        verdict, notes = parse_verdict(review)
        state['notes'] = notes
        log(f"round {round_}: review {review['ref']} verdict {verdict}: {review['front'].get('summary', '')}")
        rounds.append({'round': round_, 'implementation': impl['ref'], 'review': review['ref'], 'verdict': verdict,
                       'branch': impl['refs']['branch'], 'commits': impl['refs'].get('commits') or []})
        if verdict == 'approve':
            break


def summary(rounds, notes, outcome, error=None):
    last = rounds[-1] if rounds else {}
    result = {'outcome': outcome, 'rounds': len(rounds), 'verdict': last.get('verdict'), 'branch': last.get('branch'),
              'commit': (last.get('commits') or [None])[-1], 'implementation': last.get('implementation'),
              'review': last.get('review'), 'history': rounds}
    if error:
        result['error'] = error
    if outcome == 'max-rounds':
        result['open_notes'] = notes
    print(f"review loop: {outcome} after {len(rounds)} round(s)"
          + (f"; final branch {result['branch']} at {result['commit'] or '(no commit listed)'}, verdict {result['verdict']}"
             if rounds else '') + (f"; error: {error}" if error else ''))
    for r in rounds:
        print(f"  round {r['round']}: {r['implementation']} -> {r['review']}: {r['verdict']}")
    print(json.dumps(result, ensure_ascii=False))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('--impl-hub', required=True, help='implementer hub URL, e.g. http://svc_….treer/')
    parser.add_argument('--review-hub', required=True, help='reviewer hub URL (may be the same hub)')
    parser.add_argument('--impl-template', default='implementer')
    parser.add_argument('--review-template', default='reviewer')
    parser.add_argument('--task', required=True, help='task text, or @file')
    parser.add_argument('--base-branch', required=True, help='branch the first implementation starts from')
    parser.add_argument('--repo', help='inputs.repo for both jobs')
    parser.add_argument('--max-rounds', type=int, default=4)
    parser.add_argument('--poll-seconds', type=float, default=30)
    parser.add_argument('--job-timeout-seconds', type=float, default=0, help='give up waiting for one job (0: no limit)')
    args = parser.parse_args(argv)
    if args.max_rounds < 1:
        parser.error('--max-rounds must be at least 1')
    state = {'rounds': [], 'notes': ''}
    try:
        run(args, state)
    except LoopError as error:
        summary(state['rounds'], state['notes'], 'error', str(error))
        return 1
    except KeyboardInterrupt:
        summary(state['rounds'], state['notes'], 'error', 'interrupted (running jobs keep running)')
        return 1
    rounds, notes = state['rounds'], state['notes']
    if rounds and rounds[-1]['verdict'] == 'approve':
        summary(rounds, notes, 'approved')
        return 0
    summary(rounds, notes, 'max-rounds')
    return 2


if __name__ == '__main__':
    sys.exit(main())
