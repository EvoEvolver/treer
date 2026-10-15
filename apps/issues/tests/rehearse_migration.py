#!/usr/bin/env python3
"""Hermetic rehearsal: migrate a live-style Issues App to scripts/app-deploy version directories.

Everything is private: a temporary "machine", a stateful FAKE `treer` that really starts/stops the App process (only
PIDs it started itself), a temporary SQLite database, a private port and a tiny HTTP proxy standing in for the Treer
service route. The real scripts/app-deploy and the real Issues server are exercised. Run:

    python3 apps/issues/tests/rehearse_migration.py [--keep]

It prints every command the real migration uses, with its rollback point, and exits non-zero on any failed check.
Not part of the unit test discovery (name does not start with test_).
"""
import hashlib
import http.server
import json
import os
import shutil
import signal
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
APP = HERE.parent
REPO = APP.parents[1]
DEPLOY = REPO / 'scripts/app-deploy'
PY = str(Path(sys.executable).resolve())
sys.path.insert(0, str(APP))
sys.path.insert(0, str(HERE))
from hermetic import hermetic_env

FAKE_TREER = r'''#!%(py)s
"""Stateful fake `treer` for the migration rehearsal: interface contract, app show/list/stop/start/update."""
import json, os, signal, subprocess, sys, time
from pathlib import Path
STATE = Path(os.environ["REHEARSAL_STATE"])
state = json.loads(STATE.read_text())
app = state["app"]
args = sys.argv[1:]
def alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except OSError:
        return False
def save():
    STATE.write_text(json.dumps(state))
def show():
    running = bool(state.get("pid")) and alive(state["pid"])
    value = dict(app, status="running" if running else "stopped")
    print(json.dumps({"app": value}))
def stop():
    pid = state.get("pid")
    if pid and alive(pid):
        os.kill(pid, signal.SIGTERM)
        for _ in range(100):
            if not alive(pid):
                break
            time.sleep(0.05)
        else:
            os.kill(pid, signal.SIGKILL)   # only the PID this fake itself started
    state["pid"] = None
    app["desired_state"] = "stopped"
def start():
    stop()
    root = Path(state["machine_root"])
    environment = {"PATH": os.environ["PATH"], "HOME": str(root), "TREER_AGENT_ID": app["runtime_agent_id"],
                   "ISSUES_LISTEN": "127.0.0.1:%%d" %% state["port"]}
    log = open(root / "app.log", "ab")
    process = subprocess.Popen([app["command"], *app["args"]], cwd=str(root / app["cwd"]), env=environment, stdout=log, stderr=log, stdin=subprocess.DEVNULL, start_new_session=True)
    state["pid"] = process.pid
    app["desired_state"] = "running"
if args[:2] == ["interface", "contract"]:
    print(json.dumps({"machine": {"root": state["machine_root"], "server_id": app["server_id"], "workspace_id": "ws_rehearsal"}}))
elif args[:2] == ["app", "show"]:
    show()
elif args[:2] == ["app", "list"]:
    print(json.dumps({"apps": [dict(app)]}))
elif args[:2] == ["app", "stop"]:
    stop(); save(); show()
elif args[:2] == ["app", "start"]:
    start(); save(); show()
elif args[:2] == ["app", "update"]:
    rest = args[3:]
    was_running = bool(state.get("pid")) and alive(state["pid"])
    if "--" in rest:
        marker = rest.index("--")
        new_args, rest = rest[marker + 1:], rest[:marker]
        app["args"] = new_args
    if "--clear-args" in rest:
        app["args"] = []
    if "--cwd" in rest:
        app["cwd"] = rest[rest.index("--cwd") + 1]
    if "--command" in rest:
        app["command"] = rest[rest.index("--command") + 1]
    if was_running:
        start()          # "replace its running process"
    save(); show()
else:
    print(json.dumps({"error": {"code": "fake_unknown", "message": " ".join(args)}}), file=sys.stderr)
    sys.exit(1)
'''


class Rehearsal:
    def __init__(self, keep):
        self.keep = keep
        self.temp = tempfile.mkdtemp(prefix='issues-rehearsal-')
        self.machine = Path(self.temp, 'machine').resolve()
        self.machine.mkdir()
        self.state_file = Path(self.temp, 'treer-state.json')
        self.treer = Path(self.temp, 'treer')
        self.deploy_root = self.machine / 'apps-deployed/issues'
        self.old_db = self.machine / '.local/share/treer/apps/issues/issues.sqlite3'
        self.port = self.free_port()
        self.proxy_port = self.free_port()
        self.steps = []
        self.failures = []
        self.proxy = None

    @staticmethod
    def free_port():
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            return sock.getsockname()[1]

    # -- plumbing -----------------------------------------------------------------------------

    def say(self, text=''):
        print(text, flush=True)

    def step(self, title):
        self.say(f'\n=== {title}')

    def check(self, condition, what):
        self.say(f"  {'ok  ' if condition else 'FAIL'} {what}")
        if not condition:
            self.failures.append(what)

    def environment(self):
        env = hermetic_env({'REHEARSAL_STATE': str(self.state_file), 'PATH': os.environ['PATH'], 'HOME': str(self.machine),
                            'http_proxy': f'http://127.0.0.1:{self.proxy_port}'})
        env.pop('ISSUES_DB', None)
        return env

    def run(self, argv, show=None, check=True, timeout=180, cwd=None):
        shown = show if show is not None else ' '.join(str(a) for a in argv)
        self.say(f'  $ {shown}')
        result = subprocess.run([str(a) for a in argv], capture_output=True, text=True, env=self.environment(), timeout=timeout, cwd=cwd)
        if check and result.returncode != 0:
            self.say(result.stdout[-1500:]); self.say(result.stderr[-1500:])
            raise SystemExit(f'command failed ({result.returncode}): {shown}')
        return result

    def fake(self, *args):
        result = self.run([self.treer, *args], show='treer ' + ' '.join(args))
        return json.loads(result.stdout)

    def http(self, url, via_proxy=False, timeout=3):
        # The route stands in for http://issues.treer/ reached through the workload's HTTP proxy.
        handlers = [urllib.request.ProxyHandler({'http': f'http://127.0.0.1:{self.proxy_port}'} if via_proxy else {})]
        try:
            with urllib.request.build_opener(*handlers).open(url, timeout=timeout) as response:
                return response.status, json.loads(response.read())
        except Exception:
            return None, None

    def wait_health(self, path, want, route=False, seconds=15):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            status, body = self.http(f'http://127.0.0.1:{self.port}/{path}')
            if status == 200 and all(body.get(k) == v for k, v in want.items()):
                if not route:
                    return body
                routed_status, routed = self.http(f'http://issues.treer/{path}', via_proxy=True)
                if routed_status == 200 and all(routed.get(k) == v for k, v in want.items()):
                    return body
            time.sleep(0.2)
        return None

    def database_hash(self, path):
        with sqlite3.connect(Path(path).as_uri() + '?mode=ro', uri=True) as db:
            digest = hashlib.sha256()
            counts = {}
            for table in ('issues', 'activity', 'reminders', 'notify_windows', 'notify_budget'):
                try:
                    rows = db.execute(f'SELECT * FROM {table} ORDER BY 1,2').fetchall()
                except sqlite3.OperationalError:
                    continue
                counts[table] = len(rows)
                digest.update(repr(rows).encode())
            return digest.hexdigest(), counts

    def sha(self, path):
        return hashlib.sha256(Path(path).read_bytes()).hexdigest()

    # -- setup --------------------------------------------------------------------------------

    def setup(self):
        self.step('SETUP  a private "machine": old checkout layout, live-style database, fake treer, route proxy')
        old = self.machine / 'treer/apps'
        old.mkdir(parents=True)
        shutil.copytree(APP, old / 'issues', ignore=shutil.ignore_patterns('node_modules', '__pycache__', '*.sqlite3*'))
        from store import Store
        store = Store(self.old_db)
        directory = [{'id': 'ag_a', 'kind': 'agent', 'name': 'a'}, {'id': 'usr_h', 'kind': 'human', 'name': 'h', 'role': 'admin'}]
        actor = {'id': 'usr_h', 'kind': 'human', 'role': 'admin', 'name': 'h'}
        for n in range(40):
            issue = store.mutate(None, 'create', {'title': f'Issue {n}', 'description': 'd' * 50, 'labels': ['x'], 'assignee': 'ag_a'}, actor, directory)[0]
            store.mutate(issue, 'comment', {'body': f'comment {n}'}, actor, directory)
        self.old_hash, self.old_counts = self.database_hash(self.old_db)
        self.say(f'  old database: {self.old_counts}')
        self.treer.write_text(FAKE_TREER % {'py': PY})
        self.treer.chmod(0o755)
        spec = {'app_id': 'app_rehearsal', 'runtime_agent_id': 'app_rehearsal', 'service_id': 'svc_rehearsal', 'server_id': 'srv_test',
                'hostname': 'svc-rehearsal.internal', 'name': 'issues', 'port': 9440, 'desired_state': 'stopped',
                'command': 'env', 'args': ['ISSUES_PROXY_URL=http://127.0.0.1:9/', f'ISSUES_DB={self.old_db}', 'python3', 'apps/issues/issues.py'], 'cwd': 'treer'}
        self.state_file.write_text(json.dumps({'machine_root': str(self.machine), 'port': self.port, 'app': spec, 'pid': None}))
        self.legacy = dict(spec)
        port = self.port

        class Proxy(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                try:
                    with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(f'http://127.0.0.1:{port}{self.path[self.path.index("/", 8):]}', timeout=3) as r:
                        data = r.read(); self.send_response(r.status)
                except Exception:
                    data = b'{}'; self.send_response(502)
                self.send_header('Content-Length', str(len(data))); self.end_headers(); self.wfile.write(data)
        self.proxy = http.server.ThreadingHTTPServer(('127.0.0.1', self.proxy_port), Proxy)
        threading.Thread(target=self.proxy.serve_forever, daemon=True).start()

    # -- the procedure --------------------------------------------------------------------------

    def source_commit(self):
        self.repo_commit = subprocess.run(['git', '-C', str(REPO), 'rev-parse', 'HEAD'], capture_output=True, text=True, check=True).stdout.strip()
        self.say(f'  source commit being deployed: {self.repo_commit}')

    def step1_old_layout(self):
        self.step('STEP 0  the current live layout is running (command=env ..., cwd=<main checkout>)')
        self.fake('app', 'start', 'issues')
        body = self.wait_health('health', {'status': 'ok'})
        self.check(body is not None, 'old layout answers /health')

    def step2_prepare(self):
        self.step('STEP 1  prepare the version directory (NO downtime; the live App is untouched)')
        self.say('  rollback point: delete the apps-deployed/issues directory; nothing else changed')
        self.run([PY, DEPLOY, 'init', '--root', self.deploy_root, '--machine-root', self.machine, '--command', PY, '--entry', 'issues.py',
                  '--db-env', 'ISSUES_DB', '--', '--proxy-url', 'http://127.0.0.1:9/'], show='python3 scripts/app-deploy init --root <ROOT> --machine-root <MACHINE> --command <python3> --entry issues.py --db-env ISSUES_DB -- --proxy-url <PROXY URL>')
        check = json.dumps([PY, '-m', 'unittest', 'discover', '-s', 'tests', '-p', 'test_*.py'])
        self.run([PY, DEPLOY, 'stage', '--root', self.deploy_root, '--repo', REPO, '--commit', self.repo_commit, '--component', 'apps/issues',
                  '--check-json', check, '--check-timeout', '300'], show='python3 scripts/app-deploy stage --root <ROOT> --repo <repo> --commit <FULL SHA> --component apps/issues --check-json \'["<python3>","-m","unittest","discover","-s","tests","-p","test_*.py"]\'', timeout=400)
        self.run([PY, DEPLOY, 'bootstrap', '--root', self.deploy_root, '--release', self.repo_commit], show='python3 scripts/app-deploy bootstrap --root <ROOT> --release <FULL SHA>')
        self.check((self.deploy_root / f'releases/{self.repo_commit}/issues.py').is_file(), 'release exported from the commit (tests passed in the exported tree)')
        self.check(not (self.deploy_root / f'releases/{self.repo_commit}/node_modules').exists(), 'no node_modules in the release (runtime needs no Node)')
        self.check((self.deploy_root / f'releases/{self.repo_commit}/dist/app.js').is_file(), 'the committed dist/ is part of the release')

    def cutover(self, label):
        self.step(f'{label}  cut over: stop -> copy database -> treer app update -> start -> verify  (short downtime)')
        self.say('  rollback point: `treer app update` back to the legacy command/cwd/args (old database file is never modified)')
        self.fake('app', 'stop', 'issues')
        target = self.deploy_root / 'state/app.sqlite3'
        target.parent.mkdir(mode=0o700, exist_ok=True)
        for sidecar in ('', '-wal', '-shm'):
            Path(str(target) + sidecar).unlink(missing_ok=True)
        self.say('  $ python3 <online-backup snippet from apps/issues/README: OLD_DB -> <ROOT>/state/app.sqlite3>')
        with sqlite3.connect(self.old_db.as_uri() + '?mode=ro', uri=True) as source, sqlite3.connect(target) as destination:
            source.backup(destination)
        os.chmod(target, 0o600)
        with sqlite3.connect(target) as db:
            self.check(db.execute('PRAGMA integrity_check').fetchone()[0] == 'ok', 'integrity_check of the copied database is ok')
        self.check(self.database_hash(target) == (self.old_hash, self.old_counts), 'copy is row-for-row identical to the old database')
        self.legacy_old_sha = self.sha(self.old_db)
        relative = str(self.deploy_root.relative_to(self.machine))
        self.fake('app', 'update', 'issues', '--cwd', relative, '--command', PY, '--', str(self.deploy_root / 'entry.py'), 'run', '--root', str(self.deploy_root))
        self.fake('app', 'start', 'issues')
        body = self.wait_health('deployment-health', {'version': self.repo_commit, 'app_id': 'app_rehearsal', 'ready': True, 'database_ready': True}, route=True)
        self.check(body is not None, '/deployment-health matches on loopback AND through the route (version, stable app_id, ready, database_ready)')
        shown = self.fake('app', 'show', 'issues')['app']
        self.check(shown['app_id'] == 'app_rehearsal' and shown['service_id'] == 'svc_rehearsal', 'App ID / service ID unchanged')
        self.check((self.deploy_root / 'state/app.sqlite3-wal').exists(), 'the App opened the NEW database (WAL created under state/)')
        self.check(self.sha(self.old_db) == self.legacy_old_sha, 'the old database file is untouched (it is the rollback copy)')
        self.check(self.database_hash(target) == (self.old_hash, self.old_counts), 'data intact after start')

    def rollback_to_legacy(self):
        self.step('ROLLBACK REHEARSAL  from the migrated state back to the old layout')
        self.fake('app', 'stop', 'issues')
        legacy = self.legacy
        self.fake('app', 'update', 'issues', '--cwd', legacy['cwd'], '--command', legacy['command'], '--', *legacy['args'])
        self.fake('app', 'start', 'issues')
        self.check(self.wait_health('health', {'status': 'ok'}) is not None, 'old layout is serving again')
        self.check(self.database_hash(self.old_db) == (self.old_hash, self.old_counts), 'old database intact (writes made on the new database since cutover would NOT be here)')

    def upgrade(self):
        self.step('STEP 3  an ordinary later release with app-deploy deploy (stop, SQLite backup, switch, start, health, auto-rollback)')
        clone = Path(self.temp, 'source2')
        subprocess.run(['git', 'clone', '-q', '--local', str(REPO), str(clone)], check=True, capture_output=True)
        env = {**os.environ, 'GIT_AUTHOR_NAME': 'r', 'GIT_AUTHOR_EMAIL': 'r@x', 'GIT_COMMITTER_NAME': 'r', 'GIT_COMMITTER_EMAIL': 'r@x'}
        def commit(message, edit):
            edit()
            subprocess.run(['git', '-C', str(clone), 'commit', '-qam', message], check=True, capture_output=True, env=env)
            return subprocess.run(['git', '-C', str(clone), 'rev-parse', 'HEAD'], capture_output=True, text=True, check=True).stdout.strip()
        subprocess.run(['git', '-C', str(clone), 'checkout', '-q', self.repo_commit], check=True, capture_output=True)
        readme = clone / 'apps/issues/README.md'
        second = commit('release B', lambda: readme.write_text(readme.read_text() + '\nrelease B\n'))
        def break_it():
            entry = clone / 'apps/issues/issues.py'
            entry.write_text('raise SystemExit("candidate is broken")\n')
        third = commit('release C (crashes on start)', break_it)
        trivial = json.dumps([PY, '-c', 'pass'])
        for sha in (second, third):
            self.run([PY, DEPLOY, 'stage', '--root', self.deploy_root, '--repo', clone, '--commit', sha, '--component', 'apps/issues', '--check-json', trivial], show=f'python3 scripts/app-deploy stage --root <ROOT> --repo <repo> --commit {sha[:8]}… --component apps/issues --check-json <full test command>')
        urls = ['--health-url', f'http://127.0.0.1:{self.port}/deployment-health', '--route-url', 'http://issues.treer/deployment-health']
        self.say('  rollback point: automatic (database restored from the SQLite backup, previous release restarted); failures leave a receipt')
        result = self.run([PY, DEPLOY, 'deploy', '--root', self.deploy_root, '--treer', self.treer, '--app', 'app_rehearsal', '--release', second,
                           '--operation-id', 'rehearsal-b', *urls, '--timeout', '20'], show='python3 scripts/app-deploy deploy --root <ROOT> --treer <treer> --app <APP_ID> --release <FULL SHA> --operation-id <unique> --health-url http://127.0.0.1:9440/deployment-health --route-url http://issues.treer/deployment-health --timeout 20')
        receipt = json.loads((self.deploy_root / 'receipts/rehearsal-b.json').read_text())
        self.check(receipt['status'] == 'completed', 'deploy B completed')
        self.check(self.wait_health('deployment-health', {'version': second, 'ready': True}, route=True) is not None, 'release B is live on both health surfaces')
        self.check((self.deploy_root / 'backups/rehearsal-b.sqlite3').is_file(), 'pre-deploy SQLite backup exists under backups/')
        self.check(self.database_hash(self.deploy_root / 'state/app.sqlite3') == (self.old_hash, self.old_counts), 'data intact after the upgrade')
        failed = self.run([PY, DEPLOY, 'deploy', '--root', self.deploy_root, '--treer', self.treer, '--app', 'app_rehearsal', '--release', third,
                           '--operation-id', 'rehearsal-c', *urls, '--timeout', '6'], show='python3 scripts/app-deploy deploy ... --release <BROKEN SHA> ...', check=False)
        receipt = json.loads((self.deploy_root / 'receipts/rehearsal-c.json').read_text())
        self.check(failed.returncode != 0 and receipt['status'] == 'rolled_back', f"broken release C failed and was rolled back (receipt: {receipt['status']}, phase {receipt.get('failed_phase')})")
        self.check(self.wait_health('deployment-health', {'version': second, 'ready': True}, route=True) is not None, 'release B is serving again after the automatic rollback')
        self.check(self.database_hash(self.deploy_root / 'state/app.sqlite3') == (self.old_hash, self.old_counts), 'database restored/intact after the rollback')
        self.say('  after a crash mid-deploy (no receipt completed): `python3 scripts/app-deploy recover --root <ROOT> --treer <treer> --app <APP_ID> --operation-id <same id>`')

    def step_restore_checkout(self):
        self.step('STEP 4  restore the main checkout: drop the staged apps/issues changes (rehearsed in a throwaway repo)')
        repo = Path(self.temp, 'maincheckout')
        env = {**os.environ, 'GIT_AUTHOR_NAME': 'r', 'GIT_AUTHOR_EMAIL': 'r@x', 'GIT_COMMITTER_NAME': 'r', 'GIT_COMMITTER_EMAIL': 'r@x'}
        subprocess.run(['git', 'init', '-q', str(repo)], check=True)
        (repo / 'apps/issues').mkdir(parents=True)
        (repo / 'apps/issues/issues.py').write_text('main version\n')
        (repo / 'other.txt').write_text('someone else\'s work\n')
        subprocess.run(['git', '-C', str(repo), 'add', '-A'], check=True)
        subprocess.run(['git', '-C', str(repo), 'commit', '-qm', 'main'], check=True, env=env)
        (repo / 'apps/issues/issues.py').write_text('deployed-by-checkout version\n')
        subprocess.run(['git', '-C', str(repo), 'add', 'apps/issues'], check=True)
        (repo / 'other.txt').write_text('someone else\'s UNSTAGED work\n')
        self.say('  # first confirm what would be discarded equals what main already has (here it intentionally differs):')
        diff = subprocess.run(['git', '-C', str(repo), 'diff', '--cached', '--stat', '--', 'apps/issues'], capture_output=True, text=True).stdout
        self.say('  $ git diff --cached --stat -- apps/issues   ->  ' + diff.strip().replace('\n', ' | '))
        self.say('  $ git diff HEAD --stat -- apps/issues   (must be empty/understood before restoring)')
        self.say('  $ git restore --staged --worktree -- apps/issues')
        subprocess.run(['git', '-C', str(repo), 'restore', '--staged', '--worktree', '--', 'apps/issues'], check=True)
        status = subprocess.run(['git', '-C', str(repo), 'status', '--porcelain'], capture_output=True, text=True).stdout
        self.check((repo / 'apps/issues/issues.py').read_text() == 'main version\n', 'apps/issues is back to main')
        self.check(status.strip() == 'M other.txt', "only apps/issues was touched; other people's work is untouched")

    def finish(self):
        if self.proxy:
            self.proxy.shutdown()
        try:
            state = json.loads(self.state_file.read_text())
            if state.get('pid'):
                os.kill(state['pid'], signal.SIGTERM)       # the one PID this rehearsal started
        except Exception:
            pass
        if self.keep:
            self.say(f'\n(kept {self.temp})')
        else:
            shutil.rmtree(self.temp, ignore_errors=True)


def main():
    rehearsal = Rehearsal('--keep' in sys.argv)
    try:
        rehearsal.setup()
        rehearsal.source_commit()
        rehearsal.step1_old_layout()
        rehearsal.step2_prepare()
        rehearsal.cutover('STEP 2')
        rehearsal.rollback_to_legacy()
        rehearsal.cutover('STEP 2b (re-cutover after the rollback rehearsal)')
        rehearsal.upgrade()
        rehearsal.step_restore_checkout()
    finally:
        rehearsal.finish()
    print('\nRESULT:', 'FAILED: ' + '; '.join(rehearsal.failures) if rehearsal.failures else 'all checks passed')
    return 1 if rehearsal.failures else 0


if __name__ == '__main__':
    raise SystemExit(main())
