"""Opt-in actual launcher CLI integration, all data in temporary directories.

SOUL_TEST_LAUNCHER must point to an installed `treer-agent` command or the
native launcher binary (both accept `session ...`), e.g. a fixture-harness build:
`cargo build --locked --features fixture-harness --manifest-path launchers/agent/Cargo.toml`
then launchers/agent/target/debug/treer-agent-launcher.
No inference, credentials, Agent creation, restart or real machine calls.
"""
import os
import json
import sys
import unittest
from pathlib import Path
from test_client import ClientTest, SID, project_key


@unittest.skipUnless(os.environ.get('SOUL_TEST_LAUNCHER'),'Set SOUL_TEST_LAUNCHER for actual launcher CLI integration')
class LauncherIntegration(ClientTest):
    # Inherited client tests use a contract double; below switches only selected
    # workflow cases to actual launcher code. Inherited tests are not discovered
    # here by overriding the loader at the entry point.
    def native_cli(self):
        # `session ...` goes to the actual launcher; a normal start stays the
        # echoing double so no provider process is ever launched.
        import shlex
        cli=Path(os.environ['SOUL_TEST_LAUNCHER']).resolve()
        script=self.root/'treer-agent'
        script.write_text('#!/bin/sh\nif [ "$1" = session ]; then exec '+shlex.quote(str(cli))+' "$@"; fi\nexec '
                          +shlex.quote(sys.executable)+' '+shlex.quote(str(self.root/'fake-start.py'))+' "$@"\n')
        (self.root/'fake-start.py').write_text((Path(__file__).parent/'fake_session.py').read_text())
        script.chmod(0o755)
        import shutil
        node=shutil.which('node')
        self.env['PATH']=os.environ.get('SOUL_TEST_NODE_PATH') or (str(Path(node).resolve().parent) + ':' + os.defpath if node else os.defpath)
        self.env['CLAUDE_CONFIG_DIR']=str(self.home/'.claude')
        (self.home/'.claude').mkdir(exist_ok=True)

    def test_actual_claude_export_memory_import_restart(self):
        self.native_cli()
        original=self.claude(True)
        before=original.read_bytes()
        record=self.save('claude')
        self.assertFalse(record['contains_secrets'])
        result=json.loads(self.call('run',record['soul_id']).stdout)
        workspace=Path(result['cwd'])
        # The native import selects an isolated Claude home inside the Agent's
        # dedicated directory; the source home is never reused or modified.
        config=Path(result['claude_config']).resolve()
        self.assertTrue(config.is_relative_to((self.home/'.local/state/treer-soul/ag_restore/native').resolve()))
        self.assertEqual((workspace/'CLAUDE.md').read_text(),'project instruction\n')
        project=config/'projects'/project_key(workspace)
        restored=next(project.glob('*.jsonl'))
        self.assertNotEqual(restored.stem,SID)
        (project/'memory/MEMORY.md').write_text('new memory\n')
        restored.write_text('new learning\n')
        self.call('run',record['soul_id'])
        self.assertEqual((project/'memory/MEMORY.md').read_text(),'new memory\n')
        self.assertEqual(restored.read_text(),'new learning\n')
        self.assertEqual(original.read_bytes(),before)


if __name__=='__main__':
    from fixture import sealed_environment
    import shutil
    node=shutil.which('node')
    controls={key:os.environ[key] for key in ('SOUL_TEST_LAUNCHER','SOUL_TEST_NODE_PATH') if key in os.environ}
    if node and 'SOUL_TEST_NODE_PATH' not in controls:
        controls['SOUL_TEST_NODE_PATH']=str(Path(node).resolve().parent)+':'+os.defpath
    environment=sealed_environment(controls)
    os.environ.clear();os.environ.update(environment)
    # Keep only integration test names; don't rerun inherited contract-double suite.
    suite=unittest.TestSuite(LauncherIntegration(name) for name in dir(LauncherIntegration) if name.startswith('test_actual_'))
    result=unittest.TextTestRunner(verbosity=2).run(suite)
    raise SystemExit(0 if result.wasSuccessful() else 1)
