import importlib.util
import pathlib
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('preflight', pathlib.Path(__file__).with_name('release-source-preflight.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


class PreflightTests(unittest.TestCase):
    def test_source_and_feature_refusals(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            def git(*args):
                return subprocess.check_output(['git', '-C', tmp, *args], stderr=subprocess.DEVNULL, text=True).strip()
            git('init', '-q'); git('config', 'user.email', 'fixture@example.invalid'); git('config', 'user.name', 'fixture')
            (root/'ui').mkdir(); (root/'ui/menu').write_text('old feature')
            git('add', '.'); git('commit', '-qm', 'live'); old = git('rev-parse', 'HEAD')
            (root/'ui/menu').write_text('old feature plus new')
            git('commit', '-qam', 'candidate'); new = git('rev-parse', 'HEAD')
            git('update-ref', 'refs/remotes/origin/main', new)
            review = dict(schema='treer.release-source-review/v1', candidate=new, previous_live_source=old,
                          previous_live_receipt='fixture deployment receipt', reviewed_by='fixture reviewer',
                          component_paths=['ui/'], changed_paths=[dict(status='M', path='ui/menu')],
                          feature_comparison=[dict(feature='menu', outcome='retained', evidence='fixture control')])
            self.assertEqual(m.verify(root, new, review)['status'], 'SOURCE_PREFLIGHT_PASS')
            git('update-ref', 'refs/remotes/origin/main', old)
            with self.assertRaises(subprocess.CalledProcessError): m.verify(root, new, review)
            git('update-ref', 'refs/remotes/origin/main', new)
            review['changed_paths'] = []
            with self.assertRaises(ValueError): m.verify(root, new, review)
            review['changed_paths'] = [dict(status='M', path='ui/menu')]
            review['feature_comparison'][0]['outcome'] = 'removed_authorized'
            with self.assertRaises(ValueError): m.verify(root, new, review)
            review['feature_comparison'][0]['user_authorization'] = 'user decision fixture'
            m.verify(root, new, review)
            (root/'ui/menu').unlink(); git('commit', '-qam', 'delete'); deleted = git('rev-parse', 'HEAD')
            git('update-ref', 'refs/remotes/origin/main', deleted)
            review.update(candidate=deleted, changed_paths=[dict(status='D', path='ui/menu')])
            with self.assertRaises(ValueError): m.verify(root, deleted, review)
            review['deleted_path_authorizations'] = {'ui/menu': 'user decision fixture'}
            m.verify(root, deleted, review)


if __name__ == '__main__':
    unittest.main()
