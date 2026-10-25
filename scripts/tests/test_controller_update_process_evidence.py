"""Focused #222 diagnostics only; no updater/Host/PG invocation."""
import copy
import errno
import importlib.util
import json
import os
from pathlib import Path
import selectors
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

SOURCE = Path(__file__).resolve().parents[1] / "controller-update-process-evidence.py"
SPEC = importlib.util.spec_from_file_location("process_evidence", SOURCE)
evidence = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(evidence)


def identity(**changes):
    value = dict(pid=907, state="S", ppid=1, pgid=907, sid=907,
                 start_ticks=12345, proc_uid=1000, uids=[1000] * 4)
    value.update(changes)
    return value


def denied():
    return PermissionError(errno.EACCES, "synthetic")


def missing():
    return FileNotFoundError(errno.ENOENT, "synthetic")


class FakeProc:
    def __init__(self, before=None, after=None, exe="/usr/bin/python3", cwd="/unrelated"):
        self.values = [identity() if before is None else before,
                       identity() if after is None else after]
        self.links = dict(exe=exe, cwd=cwd)
        self.calls = []

    def pids(self):
        return [907]

    def owner(self, pid):
        return 1000

    def uids(self, pid):
        return [1000] * 4

    def snapshot(self, pid):
        self.calls.append("snapshot")
        value = self.values.pop(0)
        if isinstance(value, Exception):
            raise value
        return copy.deepcopy(value)

    def link(self, pid, field):
        self.calls.append(field)
        value = self.links[field]
        if isinstance(value, Exception):
            raise value
        return value


class ObserverContractTests(unittest.TestCase):
    def test_normal_subject_uid_contract_does_not_allow_root_fallback(self):
        with mock.patch.object(evidence.sys, "platform", "linux"), mock.patch.object(evidence.os, "geteuid", return_value=1000):
            evidence.collector_identity(1000, False, Path("/private/task"))
            for uid in [0, 1001]:
                with self.assertRaises(ValueError):
                    evidence.collector_identity(uid, False, Path("/private/task"))

    def test_root_observer_requires_explicit_complete_hosted_binding(self):
        root = Path("/private/tn-0123456789abcdef")
        env = dict(GITHUB_ACTIONS="true", RUNNER_ENVIRONMENT="github-hosted", TREER_DISPOSABLE_RUNNER="1", TREER_ACCEPTANCE_ROOT=str(root))
        with mock.patch.object(evidence.sys, "platform", "linux"), mock.patch.object(evidence.os, "getuid", return_value=0), mock.patch.object(evidence.os, "geteuid", return_value=0):
            with mock.patch.dict(evidence.os.environ, env, clear=True):
                evidence.collector_identity(1000, True, root)
                for uid, explicit, path in [(0, True, root), (1000, False, root), (1000, True, Path("/private/other"))]:
                    with self.assertRaises(ValueError):
                        evidence.collector_identity(uid, explicit, path)
            for missing in env:
                with mock.patch.dict(evidence.os.environ, {k:v for k,v in env.items() if k != missing}, clear=True):
                    with self.assertRaises(ValueError):
                        evidence.collector_identity(1000, True, root)
        with mock.patch.object(evidence.sys, "platform", "linux"), mock.patch.object(evidence.os, "getuid", return_value=1000), mock.patch.object(evidence.os, "geteuid", return_value=1000), mock.patch.dict(evidence.os.environ, env, clear=True):
            with self.assertRaises(ValueError):
                evidence.collector_identity(1000, True, root)


class ProcessEvidenceTests(unittest.TestCase):
    def observe(self, proc):
        return evidence.observe(proc, 907, "/private/task", 1000)

    def test_denied_live_field_is_unknown_with_identity_and_other_field_retained(self):
        row = self.observe(FakeProc(exe=denied()))
        self.assertEqual(row["classification"], "unknown_links")
        self.assertEqual(row["fields"]["exe"], dict(status="denied", errno=errno.EACCES))
        self.assertEqual(row["fields"]["cwd"]["status"], "ok")
        self.assertEqual(row["before"]["value"]["start_ticks"], 12345)
        self.assertEqual(row["after"]["value"]["ppid"], 1)
        self.assertNotIn("unrelated", json.dumps(row))

    def test_same_uid_is_not_task_membership(self):
        result = evidence.collect(FakeProc(), "/private/task", 1000)
        self.assertEqual(result["processes"][0]["classification"], "no_exe_cwd_root_reference")
        self.assertTrue(result["root_references_proven_absent"])
        self.assertFalse(result["task_membership_inferred_from_uid"])

    def test_partial_holder_is_not_hidden_by_other_denial(self):
        result = evidence.collect(FakeProc(exe=denied(), cwd="/private/task/worker"), "/private/task", 1000)
        self.assertEqual(result["holders"], [907])
        self.assertFalse(result["root_references_proven_absent"])
        self.assertEqual(result["processes"][0]["fields"]["exe"]["status"], "denied")

    def test_only_stable_kernel_dead_state_establishes_exit(self):
        row = self.observe(FakeProc(identity(state="Z"), identity(state="Z"), denied(), missing()))
        self.assertEqual(row["classification"], "kernel_exited")
        row = self.observe(FakeProc(identity(state="S"), identity(state="Z"), denied(), denied()))
        self.assertEqual(row["classification"], "unknown_links")

    def test_absent_link_does_not_establish_exit(self):
        self.assertEqual(self.observe(FakeProc(exe=missing(), cwd=missing()))["classification"], "unknown_links")

    def test_missing_post_identity_is_explicit_disappearance(self):
        row = self.observe(FakeProc(after=missing(), exe=denied()))
        self.assertEqual(row["classification"], "absent_after")
        self.assertEqual(row["after"]["status"], "absent")
        self.assertEqual(row["fields"]["exe"]["status"], "denied")

    def test_reuse_uid_change_and_identity_denial_cannot_pass(self):
        for after in [identity(start_ticks=54321), identity(proc_uid=0), identity(uids=[0] * 4)]:
            self.assertEqual(self.observe(FakeProc(after=after))["classification"], "unknown_identity_changed")
        self.assertEqual(self.observe(FakeProc(before=denied()))["classification"], "unknown_identity")

    def test_both_fields_and_post_identity_are_always_attempted(self):
        proc = FakeProc(exe=denied(), cwd=denied())
        result = evidence.collect(proc, "/private/task", 1000)
        self.assertEqual(proc.calls, ["snapshot", "exe", "cwd", "snapshot"])
        self.assertEqual(result["unknown"], [907])
        self.assertFalse(result["root_references_proven_absent"])

    def test_scope_uid_handles_nondumpable_root_owned_proc_directory(self):
        proc = FakeProc(identity(proc_uid=0), identity(proc_uid=0), denied(), denied())
        proc.owner = lambda pid: 0
        result = evidence.collect(proc, "/private/task", 1000)
        self.assertEqual(result["unknown"], [907])
        self.assertEqual(result["excluded_proven_other_uid"], 0)

    def test_scope_errors_remain_unknown_and_other_uid_needs_two_readable_proofs(self):
        proc = FakeProc()
        proc.owner = lambda pid: 0
        def bad(pid):
            raise denied()
        proc.uids = bad
        self.assertEqual(evidence.collect(proc, "/private/task", 1000)["unknown"], [907])
        proc.uids = lambda pid: [0] * 4
        result = evidence.collect(proc, "/private/task", 1000)
        self.assertEqual(result["excluded_proven_other_uid"], 1)
        self.assertEqual(result["processes"], [])

    def test_stat_parser_handles_comm_and_rejects_truncation(self):
        # state field3, ppid4, pgrp5, session6, starttime22.
        raw = b"907 (name ) with\nspaces) S 1 907 907 " + b"0 " * 15 + b"12345 0"
        self.assertEqual(evidence.parse_stat(raw, 907)["start_ticks"], 12345)
        for bad in [raw.replace(b"907 (", b"908 (", 1), b"907 (short) S", b"x" * 4097]:
            with self.assertRaises(evidence.EvidenceError):
                evidence.parse_stat(bad, 907)

    def test_boundary_and_deleted_executable_are_classified_without_raw_paths(self):
        self.assertFalse(evidence.within("/private/task-other", "/private/task"))
        self.assertTrue(evidence.within("/private/task/bin/worker (deleted)", "/private/task"))
        proc = FakeProc(exe="/private/task/bin/worker (deleted)")
        row = self.observe(proc)
        self.assertEqual(row["classification"], "task_root_holder")
        self.assertNotIn("worker", json.dumps(row))

    def test_bounded_pid_admission(self):
        proc = FakeProc()
        proc.pids = lambda: range(evidence.MAX_PIDS + 1)
        with self.assertRaises(evidence.EvidenceError):
            evidence.collect(proc, "/private/task", 1000)

    @unittest.skipUnless(os.environ.get("TREER_PROC_EVIDENCE_ACTUAL") == "1", "explicit fresh hosted Linux diagnostic only")
    def test_actual_owned_child_holder_then_exit(self):
        self.assertEqual(sys.platform, "linux")
        self.assertNotEqual(os.geteuid(), 0)
        self.assertEqual(os.environ.get("GITHUB_ACTIONS"), "true")
        self.assertEqual(os.environ.get("RUNNER_ENVIRONMENT"), "github-hosted")
        # New root, no acceptance inputs, old root, binary install or credentials.
        for key in ("TREER_ACCEPTANCE_ROOT", "TREER_AGENT_ID", "TREER_WORKLOAD_CREDENTIAL", "TREER_OPERATOR_CREDENTIAL"):
            self.assertNotIn(key, os.environ)
        with tempfile.TemporaryDirectory(prefix="treer222-proc-evidence-", dir=Path.home()) as root:
            child = subprocess.Popen([sys.executable, "-I", "-c", "import time; time.sleep(60)"], cwd=root)
            proc = evidence.Proc()
            try:
                row = evidence.observe(proc, child.pid, root, os.geteuid())
                self.assertEqual(row["classification"], "task_root_holder")
                born = row["before"]["value"]["start_ticks"]
                self.assertEqual(row["after"]["value"]["start_ticks"], born)
            finally:
                # Only the Popen-owned child; never a scan-derived PID or group.
                child.terminate()
                child.wait(timeout=5)
            final = evidence.observe(proc, child.pid, root, os.geteuid())
            self.assertEqual(final["classification"], "absent_after")
            # A known own child can deliberately deny exe/cwd inspection. This
            # must be retained as unknown, even if /proc directory owner is 0.
            protected = subprocess.Popen([
                sys.executable, "-I", "-c",
                "import ctypes,time; assert ctypes.CDLL(None).prctl(4,0,0,0,0)==0; print('ready',flush=True); time.sleep(60)"
            ], cwd=root, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
            try:
                with selectors.DefaultSelector() as selector:
                    selector.register(protected.stdout, selectors.EVENT_READ)
                    self.assertTrue(selector.select(timeout=5), "owned child readiness deadline")
                    self.assertEqual(os.read(protected.stdout.fileno(), 6), b"ready\n")
                unreadable = evidence.observe(proc, protected.pid, root, os.geteuid())
                self.assertEqual(unreadable["classification"], "unknown_links")
                self.assertIn(os.geteuid(), unreadable["after"]["value"]["uids"])
                self.assertEqual(unreadable["fields"]["exe"]["status"], "denied")
                self.assertEqual(unreadable["fields"]["cwd"]["status"], "denied")
            finally:
                protected.terminate()
                protected.wait(timeout=5)
                protected.stdout.close()
            protected_final = evidence.observe(proc, protected.pid, root, os.geteuid())
            self.assertEqual(protected_final["classification"], "absent_after")
            print(json.dumps({"schema": "treer.proc-root-evidence-fixture/v1", "initial": row,
                              "final": final, "owned_child_wait_exit": child.returncode,
                              "unreadable_owned_child": unreadable, "unreadable_child_final": protected_final,
                              "unreadable_child_wait_exit": protected.returncode,
                              "no_update_or_host": True}, sort_keys=True))


if __name__ == "__main__":
    unittest.main()
