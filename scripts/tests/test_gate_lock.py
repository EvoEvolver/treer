import calendar
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


    # --- FIFO queue (#174) ---

    def wait_for_tickets(self, count, timeout=15):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if len(list((self.directory / "queue").glob("*.json"))) >= count:
                return
            time.sleep(0.02)
        self.fail(f"expected {count} queued tickets, found {len(list((self.directory / 'queue').glob('*.json')))}")

    def appender(self, tag, order, hold=0.15):
        return "import time;open(r'%s','a').write('%s\\n');time.sleep(%s)" % (order, tag, hold)

    def test_authors_get_the_slot_in_arrival_order(self):
        order = self.directory / "order"
        holder = run(self.directory, sys.executable, "-c", "import time;time.sleep(1.5)", name="holder")
        time.sleep(0.4)
        waiters = []
        for index in range(4):
            waiters.append(run(self.directory, sys.executable, "-c", self.appender(f"w{index}", order), name=f"w{index}"))
            self.wait_for_tickets(index + 1)
        for proc in [holder, *waiters]:
            _, err = proc.communicate(timeout=60)
            self.assertEqual(proc.returncode, 0, err)
        self.assertEqual(order.read_text().split(), ["w0", "w1", "w2", "w3"])

    def test_release_gates_preempt_waiting_authors_and_keep_their_own_arrival_order(self):
        order = self.directory / "order"
        holder = run(self.directory, sys.executable, "-c", "import time;time.sleep(1.5)", name="holder")
        time.sleep(0.4)
        procs = []
        for index, (role, tag) in enumerate([("author", "a1"), ("author", "a2"), ("release", "r1"), ("release", "r2")]):
            procs.append(run(self.directory, sys.executable, "-c", self.appender(tag, order), role=role, name=tag))
            self.wait_for_tickets(index + 1)
        for proc in [holder, *procs]:
            _, err = proc.communicate(timeout=60)
            self.assertEqual(proc.returncode, 0, err)
        self.assertEqual(order.read_text().split(), ["r1", "r2", "a1", "a2"])

    def test_a_dead_waiters_ticket_is_removed_and_does_not_block(self):
        dead = subprocess.Popen([sys.executable, "-c", "pass"])
        dead.wait()
        queue = self.directory / "queue"
        queue.mkdir()
        ticket = queue / f"000000000001-{dead.pid}.json"
        ticket.write_text(json.dumps({"pid": dead.pid, "name": "dead", "role": "author", "since": "x", "seq": 1}))
        (self.directory / "queue.seq").write_text("1")
        proc = run(self.directory, sys.executable, "-c", "print('ok')")
        out, err = proc.communicate(timeout=20)
        self.assertEqual((proc.returncode, out.strip()), (0, "ok"), err)
        self.assertFalse(ticket.exists())

    def test_a_ticket_that_stopped_refreshing_expires_even_if_its_pid_is_alive(self):
        # a reused pid or a hung waiter must not hold the head of the queue forever
        queue = self.directory / "queue"
        queue.mkdir()
        ticket = queue / f"000000000001-{os.getpid()}.json"
        ticket.write_text(json.dumps({"pid": os.getpid(), "name": "hung", "role": "author", "since": "x", "seq": 1}))
        old = time.time() - 3600
        os.utime(ticket, (old, old))
        (self.directory / "queue.seq").write_text("1")
        proc = run(self.directory, sys.executable, "-c", "print('ok')")
        out, err = proc.communicate(timeout=20)
        self.assertEqual((proc.returncode, out.strip()), (0, "ok"), err)
        self.assertFalse(ticket.exists())

    def test_a_killed_waiter_ahead_of_you_does_not_block_the_queue(self):
        order = self.directory / "order"
        holder = run(self.directory, sys.executable, "-c", "import time;time.sleep(1.5)", name="holder")
        time.sleep(0.4)
        first = run(self.directory, sys.executable, "-c", self.appender("first", order), name="first")
        self.wait_for_tickets(1)
        second = run(self.directory, sys.executable, "-c", self.appender("second", order), name="second")
        self.wait_for_tickets(2)
        os.killpg(first.pid, 9)  # the waiter dies without cleaning up its ticket
        first.communicate()
        for proc in (holder, second):
            _, err = proc.communicate(timeout=60)
            self.assertEqual(proc.returncode, 0, err)
        self.assertEqual(order.read_text().split(), ["second"])

    def test_status_shows_holder_and_the_queue_in_order(self):
        script = [sys.executable, str(SCRIPT), "--status"]
        idle = subprocess.run(script, env=env(self.directory), capture_output=True, text=True, timeout=20)
        self.assertEqual(idle.returncode, 0)
        self.assertIn("holder: none (slot free)", idle.stdout)
        self.assertIn("queue: empty", idle.stdout)
        holder = run(self.directory, sys.executable, "-c", "import time;time.sleep(2.5)", name="holder")
        time.sleep(0.5)
        w1 = run(self.directory, sys.executable, "-c", "print(1)", name="w1")
        self.wait_for_tickets(1)
        w2 = run(self.directory, sys.executable, "-c", "print(2)", role="release", name="w2")
        self.wait_for_tickets(2)
        text = subprocess.run(script, env=env(self.directory), capture_output=True, text=True, timeout=20).stdout
        data = json.loads(subprocess.run([*script, "--json"], env=env(self.directory), capture_output=True, text=True, timeout=20).stdout)
        for proc in (holder, w1, w2):
            proc.communicate(timeout=60)
        self.assertIn("holder: holder (pid ", text)
        self.assertIn("queue: 2 waiting", text)
        # the release waiter is first although it arrived second
        self.assertLess(text.index("1. w2"), text.index("2. w1"))
        self.assertEqual([item["name"] for item in data["queue"]], ["w2", "w1"])
        self.assertEqual(data["holder"]["name"], "holder")

    def test_tickets_are_removed_when_the_gate_has_run(self):
        proc = run(self.directory, sys.executable, "-c", "print('x')")
        proc.communicate(timeout=20)
        self.assertEqual(list((self.directory / "queue").glob("*.json")), [])
        self.assertTrue((self.directory / "queue.seq").exists())


    def test_holder_since_is_the_acquisition_time_not_the_start_of_the_wait(self):
        holder = run(self.directory, sys.executable, "-c", "import time;time.sleep(2.2)", name="holder")
        time.sleep(0.4)
        started = time.time()
        code = ("import json,os,sys;print(json.dumps(json.load(open(os.path.join(os.environ['TREER_GATE_DIR'],'gate.holder.json')))))")
        waiter = run(self.directory, sys.executable, "-c", code, name="waiter")
        out, err = waiter.communicate(timeout=30)
        holder.communicate(timeout=30)
        self.assertEqual(waiter.returncode, 0, err)
        info = json.loads(out)
        parse = lambda text: calendar.timegm(time.strptime(text, "%Y-%m-%dT%H:%M:%SZ"))
        self.assertGreaterEqual(parse(info["since"]) - parse(info["queued_since"]), 1, info)
        self.assertGreaterEqual(parse(info["since"]), started - 1)
        self.assertEqual(info["name"], "waiter")


if __name__ == "__main__":
    unittest.main()
