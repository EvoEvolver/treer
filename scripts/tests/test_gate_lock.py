import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "gate-lock.py"


def env(directory, **extra):
    base = {key: value for key, value in os.environ.items() if key not in ("CI", "TREER_GATE_HELD", "TREER_GATE_LOCK", "TREER_GATE_ROLE")}
    base.update(TREER_GATE_DIR=str(directory), TREER_GATE_POLL_SECONDS="0.05")
    base.update(extra)
    return base


def run(directory, *command, role="author", name="t", **extra):
    return subprocess.Popen([sys.executable, str(SCRIPT), "--role", role, "--name", name, "--", *command],
                            env=env(directory, **extra), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                            start_new_session=True)


class GateLockTest(unittest.TestCase):
    def setUp(self):
        self.directory = Path(tempfile.mkdtemp(prefix="gate-lock-test-"))
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", str(self.directory)]))

    def test_exit_code_and_output_pass_through_and_nested_calls_do_not_deadlock(self):
        first = run(self.directory, sys.executable, "-c", "import os,sys;print(os.environ['TREER_GATE_HELD']);sys.exit(7)")
        out, _ = first.communicate(timeout=20)
        self.assertEqual((first.returncode, out.strip()), (7, "1"))
        nested = run(self.directory, sys.executable, str(SCRIPT), "--", sys.executable, "-c", "print('inner')")
        out, _ = nested.communicate(timeout=20)
        self.assertEqual((nested.returncode, out.strip()), (0, "inner"))

    def test_two_author_gates_never_overlap(self):
        marker = self.directory / "overlap"
        code = ("import os,time;p='%s';assert not os.path.exists(p);open(p,'w').close();time.sleep(0.6);os.remove(p)" % marker)
        procs = [run(self.directory, sys.executable, "-c", code, name=f"a{i}") for i in range(3)]
        for proc in procs:
            _, err = proc.communicate(timeout=60)
            self.assertEqual(proc.returncode, 0, err)

    def test_waiting_author_names_the_holder(self):
        holder = run(self.directory, sys.executable, "-c", "import time;time.sleep(1.5)", name="holder")
        time.sleep(0.5)
        waiter = run(self.directory, sys.executable, "-c", "print('ran')", name="waiter")
        _, err = waiter.communicate(timeout=30)
        holder.communicate(timeout=30)
        self.assertEqual(waiter.returncode, 0)
        self.assertIn("waiting for gate slot held by holder", err)

    def test_release_gate_goes_first_and_authors_wait_for_it(self):
        order = self.directory / "order"
        append = lambda tag: "import time;open(r'%s','a').write('%s\\n');time.sleep(0.4)" % (order, tag)
        holder = run(self.directory, sys.executable, "-c", "import time;time.sleep(1.2)", name="holder")
        time.sleep(0.4)
        author = run(self.directory, sys.executable, "-c", append("author"), name="author")
        time.sleep(0.2)
        release = run(self.directory, sys.executable, "-c", append("release"), role="release", name="release")
        for proc in (holder, author, release):
            _, err = proc.communicate(timeout=60)
            self.assertEqual(proc.returncode, 0, err)
        self.assertEqual(order.read_text().split(), ["release", "author"])
        self.assertFalse((self.directory / "release-wanted.json").exists())

    def test_stale_release_marker_of_a_dead_process_does_not_block_authors(self):
        dead = subprocess.Popen([sys.executable, "-c", "pass"])
        dead.wait()
        (self.directory / "release-wanted.json").write_text(json.dumps({"pid": dead.pid, "name": "dead", "since": "x"}))
        proc = run(self.directory, sys.executable, "-c", "print('ok')")
        out, err = proc.communicate(timeout=20)
        self.assertEqual((proc.returncode, out.strip()), (0, "ok"), err)

    def test_ci_and_explicit_disable_run_the_command_directly(self):
        for extra in ({"CI": "true"}, {"TREER_GATE_LOCK": "0"}):
            proc = run(self.directory, sys.executable, "-c", "import os;print('HELD' in ''.join(os.environ))", **extra)
            out, _ = proc.communicate(timeout=20)
            self.assertEqual(proc.returncode, 0)
            self.assertFalse((self.directory / "gate.holder.json").exists())

    def test_a_killed_holder_releases_the_slot(self):
        holder = run(self.directory, sys.executable, "-c", "import time;time.sleep(30)", name="doomed")
        time.sleep(0.5)
        os.killpg(holder.pid, 9)  # wrapper and its command (own session), like a crashed gate
        holder.communicate()
        proc = run(self.directory, sys.executable, "-c", "print('after')")
        out, _ = proc.communicate(timeout=20)
        self.assertEqual(out.strip(), "after")


if __name__ == "__main__":
    unittest.main()
