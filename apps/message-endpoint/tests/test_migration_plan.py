"""Offline inventory tests. All databases, credentials and identities are synthetic."""
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest

TOOL = Path(__file__).resolve().parents[1]/'tools/telegram-migration-plan.py'
spec = importlib.util.spec_from_file_location('migration_plan', TOOL)
plan = importlib.util.module_from_spec(spec)
spec.loader.exec_module(plan)

class MigrationPlan(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='migration-plan-test-')
        self.root = Path(self.temp.name).resolve();self.root.chmod(0o700)
        self.path = self.root/'legacy.sqlite3'
        self.db = sqlite3.connect(self.path);self.path.chmod(0o600)
        self.db.executescript('''CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT);
            CREATE TABLE poc_bindings(channel,account_id,conversation,target_agent_id,enabled);
            CREATE TABLE im_users(channel,account_id,user_id,enabled);
            CREATE TABLE outbound_deliveries(status,body);
            CREATE TABLE outbound_chunks(status,body);
            CREATE TABLE processed_updates(status,body);
            CREATE TABLE message_mappings(body);
            INSERT INTO metadata VALUES('bot_id','123456789'),('last_update_id','41');
            INSERT INTO poc_bindings VALUES('telegram','bot_test','-123:7','ag_recipient',1);
            INSERT INTO im_users VALUES('telegram','bot_test','456',1);
            INSERT INTO outbound_deliveries VALUES('acked','SYNTHETIC_SECRET_NOT_OUTPUT');
            INSERT INTO outbound_chunks VALUES('sent','SYNTHETIC_SECRET_NOT_OUTPUT');
            INSERT INTO processed_updates VALUES('completed','SYNTHETIC_SECRET_NOT_OUTPUT');''')
        self.db.commit()
    def tearDown(self):
        self.db.close();self.temp.cleanup()
    def test_inventory_is_not_authorization_and_never_includes_body(self):
        before = self.path.read_bytes()
        report = plan.inventory(self.path)
        self.assertFalse(report['ready_for_cutover'])
        self.assertEqual(report['proposed_next_offset'],42)
        self.assertEqual(report['bindings'][0]['topic_id'],7)
        self.assertIsNone(report['bindings'][0]['new_endpoint_id'])
        self.assertNotIn('SYNTHETIC_SECRET',json.dumps(report))
        self.assertNotIn('legacy_outbound_not_drained',report['blockers'])
        self.assertEqual(before,self.path.read_bytes())
        self.assertEqual(report,plan.inventory(self.path))
    def test_ambiguous_chunks_and_sent_but_unacked_are_not_drained(self):
        self.db.execute("INSERT INTO outbound_deliveries VALUES('sent','synthetic')")
        self.db.execute("INSERT INTO outbound_chunks VALUES('sending','synthetic')")
        self.db.execute("INSERT INTO message_mappings VALUES('synthetic')");self.db.commit()
        report = plan.inventory(self.path)
        self.assertIn('legacy_outbound_not_drained',report['blockers'])
        self.assertIn('ambiguous_inflight_send_requires_review',report['blockers'])
        self.assertIn('historical_context_mapping_requires_review',report['blockers'])
    def test_offset_absent_means_zero_but_malformed_or_overflow_rejected(self):
        self.db.execute("DELETE FROM metadata WHERE key='last_update_id'");self.db.commit()
        self.assertEqual(plan.inventory(self.path)['proposed_next_offset'],0)
        for value in ['-2','9'*25,'SYNTHETIC_TOKEN','4.2']:
            self.db.execute("INSERT OR REPLACE INTO metadata VALUES('last_update_id',?)",(value,));self.db.commit()
            with self.assertRaises(plan.Invalid):plan.inventory(self.path)
    def test_live_wal_rejected_and_closed_online_backup_includes_commits(self):
        self.db.execute('PRAGMA journal_mode=WAL')
        self.db.execute("UPDATE metadata SET value='50' WHERE key='last_update_id'");self.db.commit()
        for suffix in ['-wal','-shm']:Path(str(self.path)+suffix).chmod(0o600)
        self.db.execute("UPDATE metadata SET value='99' WHERE key='last_update_id'")
        with self.assertRaises(plan.Invalid):plan.inventory(self.path)
        self.db.rollback()
        snapshot=self.root/'snapshot.sqlite3'
        destination=sqlite3.connect(snapshot);snapshot.chmod(0o600)
        self.db.backup(destination);destination.close()
        self.assertEqual(plan.inventory(snapshot)['proposed_next_offset'],51)
    def test_closed_snapshot_rejects_any_sidecar_without_touching_it(self):
        for suffix in ['-wal','-shm','-journal']:
            side=Path(str(self.path)+suffix)
            side.write_bytes(b'SYNTHETIC_SIDE');side.chmod(0o600)
            with self.assertRaises(plan.Invalid):plan.inventory(self.path)
            self.assertEqual(side.read_bytes(),b'SYNTHETIC_SIDE')
            side.unlink()
    def test_sources_symlinks_hardlinks_and_broad_permissions_rejected(self):
        link=self.root/'link';link.symlink_to(self.path)
        with self.assertRaises(plan.Invalid):plan.inventory(link)
        link.unlink();os.link(self.path,link)
        with self.assertRaises(plan.Invalid):plan.inventory(self.path)
        link.unlink();self.path.chmod(0o644)
        with self.assertRaises(plan.Invalid):plan.inventory(self.path)
        self.path.chmod(0o600);self.root.chmod(0o755)
        with self.assertRaises(plan.Invalid):plan.inventory(self.path)
        self.root.chmod(0o700)
    def test_private_report_is_exclusive_and_never_overwrites(self):
        output=self.root/'plan.json';report=plan.inventory(self.path)
        plan.write_private(output,report)
        self.assertEqual(output.stat().st_mode&0o777,0o600)
        with self.assertRaises(FileExistsError):plan.write_private(output,report)
        link=self.root/'link';link.symlink_to(output)
        with self.assertRaises(FileExistsError):plan.write_private(link,report)
    def test_private_report_rejected_inside_an_unrelated_checkout(self):
        (self.root/'.git').mkdir()
        with self.assertRaises(plan.Invalid):plan.write_private(self.root/'report.json',plan.inventory(self.path))
        self.assertFalse((self.root/'report.json').exists())
    def test_cli_errors_hide_source_metadata_and_paths(self):
        self.db.execute("UPDATE metadata SET value='SYNTHETIC_TOKEN' WHERE key='bot_id'");self.db.commit()
        result=subprocess.run([sys.executable,'-I',str(TOOL),'--legacy-db',str(self.path),'--output',str(self.root/'out')],capture_output=True,text=True,env={'HOME':str(self.root),'PATH':'/usr/bin:/bin'})
        self.assertEqual(result.returncode,1)
        self.assertEqual(result.stderr,'migration_preflight_failed\n')
        self.assertEqual(result.stdout,'')
        self.assertFalse((self.root/'out').exists())
    def test_unknown_status_invalid_binding_or_capacity_fails_closed(self):
        self.db.execute("INSERT INTO outbound_chunks VALUES('unexpected','synthetic')");self.db.commit()
        with self.assertRaises(plan.Invalid):plan.inventory(self.path)
        self.db.execute("DELETE FROM outbound_chunks WHERE status='unexpected'")
        self.db.execute("UPDATE poc_bindings SET conversation='https://untrusted/'");self.db.commit()
        with self.assertRaises(plan.Invalid):plan.inventory(self.path)
        self.db.execute("DELETE FROM poc_bindings")
        self.db.executemany('INSERT INTO poc_bindings VALUES(?,?,?,?,?)',[('telegram','bot_test',f'{i+1}:0','ag_recipient',1) for i in range(1001)]);self.db.commit()
        with self.assertRaises(plan.Invalid):plan.inventory(self.path)

if __name__ == '__main__':unittest.main()
