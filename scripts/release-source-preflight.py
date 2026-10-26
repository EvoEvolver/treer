#!/usr/bin/env python3
"""Read-only formal-release source and previous-live comparison check (#20)."""
import argparse
import json
from pathlib import Path
import re
import subprocess
import sys


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args], text=True).strip()


def verify(root, candidate, review):
    def exact(value):
        if not isinstance(value, str) or not re.fullmatch(r'[0-9a-f]{40}', value):
            raise ValueError('source must be a full commit SHA')
        if git(root, 'rev-parse', value + '^{commit}') != value:
            raise ValueError('commit mismatch')
        return value
    candidate = exact(candidate)
    main = git(root, 'rev-parse', 'refs/remotes/origin/main^{commit}')
    subprocess.run(['git', '-C', str(root), 'merge-base', '--is-ancestor', candidate, main], check=True)
    if review.get('schema') != 'treer.release-source-review/v1' or review.get('candidate') != candidate:
        raise ValueError('missing matching source review')
    previous = exact(review['previous_live_source'])
    if not review.get('previous_live_receipt') or not review.get('reviewed_by'):
        raise ValueError('previous live receipt and named reviewer required')
    paths = review.get('component_paths')
    if not isinstance(paths, list) or not paths or any(not isinstance(p, str) or p.startswith(('/', ':')) or '..' in p.split('/') for p in paths):
        raise ValueError('explicit repository component paths required')
    # --no-renames makes deletions explicit; compare endpoints, not task ancestry.
    raw = subprocess.check_output(['git', '-C', str(root), 'diff', '--no-renames', '--name-status', '-z', previous, candidate, '--', *paths])
    fields = raw.decode().rstrip('\0').split('\0') if raw else []
    actual = [{'status': fields[i], 'path': fields[i+1]} for i in range(0, len(fields), 2)]
    if review.get('changed_paths') != actual:
        raise ValueError('complete previous-live diff not reviewed or stale')
    features = review.get('feature_comparison')
    if not isinstance(features, list) or not features:
        raise ValueError('explicit delivered-feature comparison required')
    for feature in features:
        if not feature.get('feature') or not feature.get('evidence') or feature.get('outcome') not in ('retained', 'added', 'removed_authorized'):
            raise ValueError('unresolved delivered-feature comparison')
        if feature['outcome'] == 'removed_authorized' and not feature.get('user_authorization'):
            raise ValueError('feature removal requires exact user authorization')
    approvals = review.get('deleted_path_authorizations', {})
    if any(row['status'] == 'D' and not approvals.get(row['path']) for row in actual):
        raise ValueError('deleted path requires explicit removal authorization or explained relocation')
    return {'candidate': candidate, 'origin_main': main, 'previous_live_source': previous,
            'component_paths': paths, 'changed_paths': actual, 'reviewed_by': review['reviewed_by'],
            'status': 'SOURCE_PREFLIGHT_PASS', 'deployment_authorized': False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--candidate', required=True)
    parser.add_argument('--review', required=True, type=Path)
    args = parser.parse_args()
    try:
        if not args.review.is_absolute():
            raise ValueError('review path must be absolute')
        root = Path(git(Path.cwd(), 'rev-parse', '--show-toplevel'))
        print(json.dumps(verify(root, args.candidate, json.loads(args.review.read_text())), indent=2))
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        print('source preflight refused: ' + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
