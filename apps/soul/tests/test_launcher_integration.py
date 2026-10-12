"""Opt-in actual launcher CLI integration, all data in temporary directories.

SOUL_TEST_SESSION_CLI must point to the reviewed launcher scripts/session.sh.
No inference, credentials, Agent creation, restart or real machine calls.
"""
import os
import json
import unittest
from pathlib import Path
from test_client import ClientTest, SID, project_key


@unittest.skipUnless(os.environ.get('SOUL_TEST_SESSION_CLI'),'Set SOUL_TEST_SESSION_CLI for actual launcher CLI integration')
class LauncherIntegration(ClientTest):
    # Inherited client tests use a contract double; below switches only selected
    # workflow cases to actual launcher code. Inherited tests are not discovered
    # here by overriding the loader at the entry point.
    def native_cli(self):
        cli=Path(os.environ['SOUL_TEST_SESSION_CLI']).resolve()
        script=self.root/'session.sh'
        script.write_text('#!/bin/sh\nexec '+__import__('shlex').quote(str(cli))+' "$@"\n')
        script.chmod(0o755)
        import shutil
        node=shutil.which('node')
        self.env['PATH']=os.environ.get('SOUL_TEST_NODE_PATH') or (str(Path(node).resolve().parent) + ':' + os.defpath if node else os.defpath)
        self.env['CODEX_HOME']=str(self.home/'.codex')
        self.env['CLAUDE_CONFIG_DIR']=str(self.home/'.claude')
        (self.home/'.claude').mkdir(exist_ok=True)
        (self.home/'.codex').mkdir(exist_ok=True)

    def test_actual_codex_export_redact_import_restart(self):
        self.native_cli()
        original=self.codex()
        before=original.read_bytes()
        (self.cwd/'AGENTS.md').write_text('project instructions\n')
        record=self.save('codex')
        self.assertFalse(record['contains_secrets'])
        self.assertTrue(any(f['path']=='AGENTS.md' for f in record['manifest']['adapter']['transfer_files']))
        result=json.loads(self.call('run',record['soul_id']).stdout)
        config=Path(result['codex_home'])
        self.assertTrue(config.resolve().is_relative_to((self.home/'.local/state/treer-soul/ag_restore').resolve()))
        session=next((config/'sessions').glob('*.jsonl'))
        meta=json.loads(session.read_text().splitlines()[0])['payload']
        self.assertNotEqual(meta['id'],SID)
        self.assertEqual(meta['cwd'],result['cwd'])
        session.write_text('new learning\n')
        self.call('run',record['soul_id'])
        self.assertEqual(session.read_text(),'new learning\n')
        self.assertEqual(original.read_bytes(),before)

    def test_actual_claude_export_memory_import_restart(self):
        self.native_cli()
        original=self.claude(True)
        before=original.read_bytes()
        record=self.save('claude')
        self.assertFalse(record['contains_secrets'])
        result=json.loads(self.call('run',record['soul_id']).stdout)
        workspace=Path(result['cwd'])
        self.assertEqual(Path(result['claude_config']).resolve(),(self.home/'.claude').resolve())
        self.assertEqual((workspace/'CLAUDE.md').read_text(),'project instruction\n')
        project=self.home/'.claude/projects'/project_key(workspace)
        restored=next(project.glob('*.jsonl'))
        self.assertNotEqual(restored.stem,SID)
        (project/'memory/MEMORY.md').write_text('new memory\n')
        restored.write_text('new learning\n')
        self.call('run',record['soul_id'])
        self.assertEqual((project/'memory/MEMORY.md').read_text(),'new memory\n')
        self.assertEqual(restored.read_text(),'new learning\n')
        self.assertEqual(original.read_bytes(),before)

    def test_actual_legacy_codex_snapshot_remains_readable_and_restores_without_shell(self):
        self.native_cli()
        original=self.codex()
        import io, tarfile
        soul_id='soul_'+'b'*32
        legacy=self.root/'legacy-native'
        (legacy/'souls').mkdir(parents=True)
        manifest=dict(schema_version=1,name='Old Codex',environment={},adapter=dict(name='codex',session_id=SID,
            session_file='files/session.jsonl',session_relative_path='2026/10/12/rollout-'+SID+'.jsonl',
            shell_snapshot_file='files/snapshot.sh',shell_snapshot_name=SID+'.sh'))
        with tarfile.open(legacy/'souls'/f'{soul_id}.tar','w') as archive:
            for path,data in [('manifest.json',json.dumps(manifest).encode()),('files/session.jsonl',original.read_bytes()),
                              ('files/snapshot.sh',b'echo NEVER_EXECUTE_ME\n')]:
                member=tarfile.TarInfo(path);member.size=len(data);archive.addfile(member,io.BytesIO(data))
        (legacy/'souls'/f'{soul_id}.json').write_text(json.dumps(dict(soul_id=soul_id,created_at='2025-01-01T00:00:00Z')))
        from soul import SoulStore
        self.server.store.close();self.server.store=SoulStore(legacy);self.server.auth.store=self.server.store
        record=json.loads(self.call('show',soul_id).stdout)
        self.assertEqual(record['owner'],'legacy')
        result=json.loads(self.call('run',soul_id).stdout)
        self.assertTrue(next((Path(result['codex_home'])/'sessions').glob('*.jsonl')).exists())
        self.assertFalse((Path(result['codex_home'])/'shell_snapshots').exists())


if __name__=='__main__':
    from fixture import sealed_environment
    import shutil
    node=shutil.which('node')
    controls={key:os.environ[key] for key in ('SOUL_TEST_SESSION_CLI','SOUL_TEST_NODE_PATH') if key in os.environ}
    if node and 'SOUL_TEST_NODE_PATH' not in controls:
        controls['SOUL_TEST_NODE_PATH']=str(Path(node).resolve().parent)+':'+os.defpath
    environment=sealed_environment(controls)
    os.environ.clear();os.environ.update(environment)
    # Keep only integration test names; don't rerun inherited contract-double suite.
    suite=unittest.TestSuite(LauncherIntegration(name) for name in dir(LauncherIntegration) if name.startswith('test_actual_'))
    result=unittest.TextTestRunner(verbosity=2).run(suite)
    raise SystemExit(0 if result.wasSuccessful() else 1)
