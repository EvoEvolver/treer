"""Synthetic-only checks: never reads live agent state or starts a provider."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("check_idle", Path(__file__).with_name("check-node-idle.py"))
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)


class Check(unittest.TestCase):
    def test_inert_goal_sidecar_preserved_and_pending_unknown_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory).resolve()
            for name, data in [('agent-ui.json', {'threadId': 'synthetic'}), ('queue.json', {'version': 1, 'records': []})]:
                path = source / name
                path.write_text(json.dumps(data))
                path.chmod(0o600)
            path = source / 'agent-ui.json.goal-usage.json'
            inert = dict(sessionId='old-claude', goalKey='', goal=None, pendingExit=None, tokensUsed=None, timeUsedSeconds=None, resultTokens=123)
            path.write_text(json.dumps(inert))
            path.chmod(0o600)
            before = path.read_bytes()
            result = check.inspect(source, 'synthetic')
            self.assertEqual(result['code'], 'node_idle')
            self.assertTrue(result['inertGoalUsageSidecar'])
            self.assertEqual(len(result['files']), 3)
            self.assertEqual(path.read_bytes(), before)
            for key, value in [('goal', {'status': 'active'}), ('pendingExit', {}), ('goalKey', 'old'), ('resultTokens', True), ('unknown', None)]:
                path.write_text(json.dumps({**inert, key: value}))
                with self.assertRaises(ValueError):
                    check.inspect(source, 'synthetic')

    def test_delivery_projection_excludes_prompt_and_other_content(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory).resolve()
            (source / 'agent-ui.json').write_text('{"threadId":"synthetic"}')
            (source / 'agent-ui.json').chmod(0o600)
            record = dict(submission=dict(id='sub_one', state='done', text_preview='PRIVATE-SYNTHETIC'),
                          text='PRIVATE-SYNTHETIC', notify=None, notification=None,
                          sourceMessage=dict(messageId='msg_one', deliveryId='dlv_one', ackPending=False),
                          sourceMessages=[dict(messageId='msg_two', deliveryId='dlv_two', ackPending=True)])
            data = json.dumps(dict(version=1, records=[record]))
            (source / 'queue.json').write_text(data)
            (source / 'queue.json').chmod(0o600)
            result = check.inspect(source, 'synthetic', delivery_audit=True)
            self.assertEqual(result['pendingAck'], 1)
            self.assertFalse(result['privateRootChecked'])
            self.assertEqual(len(result['deliveries'][0]['sourceMessages']), 2)
            self.assertEqual(result['deliveries'][0]['submission'], dict(id='sub_one', state='done'))
            self.assertNotIn('PRIVATE-SYNTHETIC', json.dumps(result))
            self.assertEqual((source / 'queue.json').read_text(), data)

    def test_pending_work_and_no_mutation(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory).resolve()
            ui = source / "agent-ui.json"
            ui.write_text('{"threadId":"synthetic"}')
            ui.chmod(0o600)
            queue = source / "queue.json"
            for state, notification, notify, ack, key in [
                ("running", None, None, False, "running"),
                ("queued", None, None, False, "queued"),
                ("done", None, None, True, "pendingAck"),
                ("done", {"delivered": False}, {"on": ["done"]}, False, "pendingNotification"),
                ("done", None, {"on": ["done"]}, False, "pendingNotification"),
                ("done", {"delivered": True}, {"on": ["done"]}, False, None),
            ]:
                record = dict(submission=dict(state=state), notification=notification, notify=notify,
                              sourceMessages=[dict(ackPending=ack)])
                data = json.dumps(dict(version=1, records=[record]))
                queue.write_text(data)
                queue.chmod(0o600)
                result = check.inspect(source, "synthetic")
                self.assertEqual(result["code"], "node_pending_work" if key else "node_idle")
                if key:
                    self.assertEqual(result[key], 1)
                self.assertEqual(queue.read_text(), data)
            with self.assertRaises(ValueError):
                check.inspect(source, "wrong-thread")
            queue.unlink()
            queue.symlink_to(ui)
            with self.assertRaises(OSError):
                check.inspect(source, "synthetic")

    def test_duplicate_keys_and_missing_state_fail(self):
        with self.assertRaises(ValueError):
            json.loads('{"state":"done","state":"running"}', object_pairs_hook=check.unique_pairs)

    def test_first_switch_root_and_attachments(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory).resolve()
            source, private = base / "source", base / "private"
            source.mkdir(mode=0o700)
            private.mkdir(mode=0o700)
            (private / "images").mkdir(mode=0o700)
            ui, queue = source / "agent-ui.json", source / "queue.json"
            ui.write_text('{"threadId":"synthetic"}')
            ui.chmod(0o600)
            def write(records):
                queue.write_text(json.dumps(dict(version=1, records=records)))
                queue.chmod(0o600)
            write([])
            self.assertEqual(check.inspect(source, "synthetic", private)["code"], "node_idle")
            (private / "images" / "draft").write_text("synthetic")
            with self.assertRaises(ValueError):
                check.inspect(source, "synthetic", private)
            (private / "images" / "draft").unlink()
            (private / "node-backup-v1").mkdir(mode=0o700)
            with self.assertRaises(ValueError):
                check.inspect(source, "synthetic", private)
            (private / "node-backup-v1").rmdir()
            write([dict(submission=dict(state="done"), notify=None, notification=None, attachmentIds=["synthetic"])])
            self.assertEqual(check.inspect(source, "synthetic", private)["attachmentReferences"], 1)
            write([dict(submission={}, notify=None, notification=None)])
            with self.assertRaises(ValueError):
                check.inspect(source, "synthetic", private)


if __name__ == "__main__":
    unittest.main()
