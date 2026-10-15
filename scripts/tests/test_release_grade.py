from importlib import util
from pathlib import Path
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
spec = util.spec_from_file_location("release_grade", SCRIPTS / "release-grade.py")
grade = util.module_from_spec(spec)
spec.loader.exec_module(grade)


class ReleaseGradeTest(unittest.TestCase):
    def test_single_component_grades(self):
        self.assertEqual(grade.plan(["docs/releases.md", "AGENTS.md"])["components"], ["docs"])
        self.assertEqual(grade.plan(["apps/issues/issues.py"])["components"], ["apps"])
        self.assertEqual(grade.plan(["web/src/index.css"])["components"], ["web"])
        self.assertEqual(grade.plan(["launchers/agent/src/model.ts"])["components"], ["launcher"])
        self.assertEqual(grade.plan(["crates/treer-proxy/src/lib.rs"])["components"], ["core"])

    def test_docs_and_apps_never_need_native_or_canary(self):
        for paths in (["docs/x.md"], ["apps/issues/issues.py"], [".gitattributes"]):
            result = grade.plan(paths)
            self.assertFalse(result["native_build_and_canary"], paths)
            self.assertNotIn("just check", result["local_gate"])

    def test_web_and_launcher_skip_rust_tests_but_still_ship_through_canary(self):
        for paths in (["web/src/a.tsx"], ["launchers/agent/src/a.ts"]):
            result = grade.plan(paths)
            self.assertNotIn("just check", result["local_gate"])
            self.assertTrue(result["native_build_and_canary"])
            self.assertTrue(result["policy_install_check"])

    def test_core_and_unknown_paths_run_everything_and_never_batch(self):
        for paths in (["crates/treer-cli/src/main.rs"], ["scripts/canary-release.py"], [".github/workflows/ci.yml"], ["brand-new-dir/file"], ["skills/treer/SKILL.md"]):
            result = grade.plan(paths)
            self.assertEqual(result["local_gate"], ["just check"], paths)
            self.assertTrue(result["native_build_and_canary"])
            self.assertFalse(result["batchable"])

    def test_mixed_changes_take_the_union_and_any_core_path_wins(self):
        result = grade.plan(["web/a.ts", "launchers/agent/a.ts", "apps/issues/a.py"])
        self.assertEqual(result["components"], ["apps", "web", "launcher"])
        self.assertIn("just agent-launcher-test", result["local_gate"])
        self.assertIn("cd web && pnpm test:e2e", result["local_gate"])
        self.assertEqual(grade.plan(["web/a.ts", "crates/x/src/lib.rs"])["local_gate"], ["just check"])

    def test_urgent_is_never_batched(self):
        self.assertTrue(grade.plan(["web/a.ts"])["batchable"])
        self.assertFalse(grade.plan(["web/a.ts"], urgent=True)["batchable"])

    def test_neutral_files_do_not_raise_the_grade(self):
        self.assertEqual(grade.plan([".gitattributes", "apps/issues/a.py"])["components"], ["apps"])
        self.assertEqual(grade.plan([".gitattributes"])["components"], ["docs"])

    def test_nested_path_names_do_not_match_by_substring(self):
        self.assertEqual(grade.plan(["crates/web/x.rs"])["components"], ["core"])
        self.assertEqual(grade.plan(["README.md.bak"])["components"], ["core"])
        self.assertEqual(grade.plan(["docs-extra/x"])["components"], ["core"])


if __name__ == "__main__":
    unittest.main()
