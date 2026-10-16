import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
PLATFORMS = ["linux-x86_64", "linux-aarch64", "darwin-x86_64", "darwin-aarch64", "windows-x86_64"]
BINARIES = ["treer", "treer-agent-host", "treer-agent-server"]


def git(repo, *args):
    return subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True, text=True,
                          env={**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.invalid",
                               "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.invalid"}).stdout.strip()


def commit_files(repo, files, message, remove=()):
    for name, text in files.items():
        path = repo / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    for name in remove:
        git(repo, "rm", "-q", name)
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", message)
    return git(repo, "rev-parse", "HEAD")


def build_dist(directory, commit):
    for platform in PLATFORMS:
        folder = directory / platform
        folder.mkdir(parents=True)
        for binary in BINARIES:
            path = folder / binary
            path.write_bytes((b"MZ" if platform.startswith("windows") else b"\x7fELF") + f"{platform}{binary}".encode())
            path.chmod(0o755)
        (folder / "build-metadata.json").write_text(json.dumps(
            {"schema_version": 1, "git_commit": commit, "platform": platform, "version": "1.0.0"}))
        lines = subprocess.run(["shasum", "-a", "256", *BINARIES], cwd=folder, check=True, capture_output=True, text=True).stdout
        (folder / "SHA256SUMS").write_text(lines)


class ReuseTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name) / "repo"
        self.repo.mkdir()
        git(self.repo, "init", "-q")
        (self.repo / "scripts").mkdir()
        for name in ("reuse-machine-artifacts.sh", "verify-machine-artifacts.sh", "machine-artifact-hashes.sh"):
            shutil.copy(SCRIPTS / name, self.repo / "scripts" / name)
        self.source = commit_files(self.repo, {"crates/core/lib.rs": "1", "web/index.html": "1", "apps/a/app.py": "1", "docs/x.md": "1"}, "source")
        self.checkouts = Path(self.temp.name) / "checkouts"
        self.install_source(self.source)

    def install_source(self, commit, status="canary_passed", created="2026-10-15T00:00:00Z", with_hashes=True):
        checkout = self.checkouts / commit
        shutil.rmtree(checkout / "dist", ignore_errors=True)
        build_dist(checkout / "dist", commit)
        manifest = checkout / ".treer" / "releases" / commit
        manifest.mkdir(parents=True, exist_ok=True)
        hashes = subprocess.run(["sh", str(SCRIPTS / "machine-artifact-hashes.sh"), str(checkout / "dist")], check=True,
                                capture_output=True, text=True).stdout
        record = {"status": status, "commit": commit, "created_at": created}
        if with_hashes:
            record["machine_artifacts"] = {"source_commit": commit, "reused": False, "files": json.loads(hashes)}
        (manifest / "manifest.json").write_text(json.dumps(record))

    def reuse(self, candidate=None, *extra):
        candidate = candidate or git(self.repo, "rev-parse", "HEAD")
        return subprocess.run(["sh", "scripts/reuse-machine-artifacts.sh", candidate, *extra], cwd=self.repo,
                              capture_output=True, text=True,
                              env={**os.environ, "TREER_ARTIFACT_REUSE_ROOT": str(self.checkouts)})

    def test_web_apps_docs_only_changes_reuse_the_verified_artifacts_with_provenance(self):
        candidate = commit_files(self.repo, {"web/index.html": "2", "apps/a/app.py": "2", "docs/x.md": "2"}, "ui")
        result = self.reuse()
        self.assertEqual(result.returncode, 0, result.stderr)
        provenance = json.loads((self.repo / "dist" / "artifact-provenance.json").read_text())
        self.assertEqual((provenance["source_commit"], provenance["candidate_commit"]), (self.source, candidate))
        verified = subprocess.run(["sh", "scripts/verify-machine-artifacts.sh", candidate, "dist"], cwd=self.repo, capture_output=True, text=True)
        self.assertEqual(verified.returncode, 0, verified.stderr)

    def test_the_root_readme_alone_may_change_but_no_other_markdown(self):
        candidate = commit_files(self.repo, {"README.md": "2", "web/index.html": "2"}, "readme")
        self.assertEqual(self.reuse().returncode, 0)
        verified = subprocess.run(["sh", "scripts/verify-machine-artifacts.sh", candidate, "dist"], cwd=self.repo, capture_output=True, text=True)
        self.assertEqual(verified.returncode, 0, verified.stderr)
        for path in ("README.md.bak", "crates/README.md", "launchers/agent/README.md", "CHANGELOG.md", "READMEXmd"):
            with self.subTest(path=path):
                git(self.repo, "reset", "-q", "--hard", self.source)
                shutil.rmtree(self.repo / "dist", ignore_errors=True)
                commit_files(self.repo, {path: "1", "README.md": "3"}, "other")
                self.assertNotEqual(self.reuse().returncode, 0)
                self.assertFalse((self.repo / "dist").exists())

    def test_any_native_input_change_forces_a_native_build(self):
        for files in ({"crates/core/lib.rs": "2"}, {"launchers/agent/x.ts": "1"}, {"skills/treer/SKILL.md": "1"}, {"new-top/dir/file": "1"}, {"Cargo.lock": "1"}):
            with self.subTest(files=files):
                git(self.repo, "reset", "-q", "--hard", self.source)
                shutil.rmtree(self.repo / "dist", ignore_errors=True)
                commit_files(self.repo, {**files, "web/index.html": "2"}, "mixed")
                result = self.reuse()
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((self.repo / "dist").exists())

    def test_a_rename_out_of_a_native_path_is_not_hidden_by_rename_detection(self):
        git(self.repo, "mv", "crates/core/lib.rs", "web/lib.rs")
        git(self.repo, "commit", "-q", "-m", "move")
        self.assertNotEqual(self.reuse().returncode, 0)
        self.assertFalse((self.repo / "dist").exists())

    def test_tampered_or_unverified_sources_are_not_reused(self):
        commit_files(self.repo, {"web/index.html": "2"}, "ui")
        (self.checkouts / self.source / "dist" / "linux-x86_64" / "treer").write_bytes(b"tampered")
        self.assertNotEqual(self.reuse().returncode, 0)
        self.assertFalse((self.repo / "dist").exists())

    def test_source_must_be_canary_passed_and_an_ancestor(self):
        commit_files(self.repo, {"web/index.html": "2"}, "ui")
        (self.checkouts / self.source / ".treer/releases" / self.source / "manifest.json").write_text(
            json.dumps({"status": "failed", "commit": self.source, "created_at": "x"}))
        self.assertNotEqual(self.reuse().returncode, 0)
        self.install_source(self.source)
        git(self.repo, "checkout", "-q", "--orphan", "other")
        other = commit_files(self.repo, {"web/index.html": "3"}, "unrelated")
        self.assertNotEqual(self.reuse(other).returncode, 0)

    def test_existing_dist_is_never_replaced(self):
        commit_files(self.repo, {"web/index.html": "2"}, "ui")
        (self.repo / "dist").mkdir()
        (self.repo / "dist" / "keep").write_text("x")
        self.assertNotEqual(self.reuse().returncode, 0)
        self.assertEqual((self.repo / "dist" / "keep").read_text(), "x")

    def test_newest_eligible_canary_passed_ancestor_wins(self):
        middle = commit_files(self.repo, {"web/index.html": "2"}, "ui 1")
        self.install_source(middle, created="2026-10-16T00:00:00Z")
        commit_files(self.repo, {"web/index.html": "3"}, "ui 2")
        self.assertEqual(self.reuse().returncode, 0)
        self.assertEqual(json.loads((self.repo / "dist" / "artifact-provenance.json").read_text())["source_commit"], middle)

    def test_forged_provenance_cannot_launder_a_native_change(self):
        candidate = commit_files(self.repo, {"crates/core/lib.rs": "evil"}, "native change")
        build_dist(self.repo / "dist", self.source)
        forged = {"schema_version": 1, "source_commit": self.source, "candidate_commit": candidate,
                  "files": self.expected_hashes(self.repo / "dist")}
        (self.repo / "dist" / "artifact-provenance.json").write_text(json.dumps(forged))
        result = subprocess.run(["sh", "scripts/verify-machine-artifacts.sh", candidate, "dist"], cwd=self.repo, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("native inputs changed", result.stderr)

    def test_artifacts_for_another_commit_without_provenance_stay_rejected(self):
        candidate = commit_files(self.repo, {"web/index.html": "2"}, "ui")
        build_dist(self.repo / "dist", self.source)
        result = subprocess.run(["sh", "scripts/verify-machine-artifacts.sh", candidate, "dist"], cwd=self.repo, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)

    def expected_hashes(self, directory):
        return {f"{platform}/{name}": hashlib.sha256((directory / platform / name).read_bytes()).hexdigest()
                for platform in PLATFORMS
                for name in (*BINARIES, "SHA256SUMS", "build-metadata.json")}

    def test_provenance_records_every_file_hash_and_they_match_the_copied_bytes(self):
        commit_files(self.repo, {"web/index.html": "2"}, "ui")
        self.assertEqual(self.reuse().returncode, 0)
        provenance = json.loads((self.repo / "dist" / "artifact-provenance.json").read_text())
        self.assertEqual(len(provenance["files"]), 25)
        self.assertEqual(provenance["files"], self.expected_hashes(self.repo / "dist"))
        hashes = subprocess.run(["sh", "scripts/machine-artifact-hashes.sh", "dist"], cwd=self.repo,
                                check=True, capture_output=True, text=True).stdout
        self.assertEqual(json.loads(hashes), provenance["files"])

    def test_bytes_changed_after_copy_fail_the_recorded_hashes_even_with_a_rewritten_checksum_file(self):
        candidate = commit_files(self.repo, {"web/index.html": "2"}, "ui")
        self.assertEqual(self.reuse().returncode, 0)
        folder = self.repo / "dist" / "linux-x86_64"
        (folder / "treer").write_bytes(b"\x7fELFtampered")
        sums = subprocess.run(["shasum", "-a", "256", *BINARIES], cwd=folder, check=True, capture_output=True, text=True).stdout
        (folder / "SHA256SUMS").write_text(sums)
        result = subprocess.run(["sh", "scripts/verify-machine-artifacts.sh", candidate, "dist"], cwd=self.repo, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("recorded provenance hashes", result.stderr)

    def test_consecutive_reuse_keeps_the_original_build_source(self):
        middle = commit_files(self.repo, {"web/index.html": "2"}, "ui 1")
        self.assertEqual(self.reuse().returncode, 0)
        shutil.copytree(self.repo / "dist", self.checkouts / middle / "dist")
        shutil.rmtree(self.repo / "dist")
        manifest = self.checkouts / middle / ".treer" / "releases" / middle
        manifest.mkdir(parents=True)
        recorded = json.loads(subprocess.run(["sh", str(SCRIPTS / "machine-artifact-hashes.sh"), str(self.checkouts / middle / "dist")],
                                             check=True, capture_output=True, text=True).stdout)
        (manifest / "manifest.json").write_text(json.dumps({"status": "canary_passed", "commit": middle, "created_at": "2026-10-16T00:00:00Z",
                                                            "machine_artifacts": {"source_commit": self.source, "reused": True, "files": recorded}}))
        candidate = commit_files(self.repo, {"web/index.html": "3"}, "ui 2")
        result = self.reuse()
        self.assertEqual(result.returncode, 0, result.stderr)
        provenance = json.loads((self.repo / "dist" / "artifact-provenance.json").read_text())
        self.assertEqual((provenance["source_commit"], provenance["passed_release_commit"], provenance["candidate_commit"]),
                         (self.source, middle, candidate))
        verified = subprocess.run(["sh", "scripts/verify-machine-artifacts.sh", candidate, "dist"], cwd=self.repo, capture_output=True, text=True)
        self.assertEqual(verified.returncode, 0, verified.stderr)

    def test_consecutive_reuse_is_refused_when_a_native_path_changed_since_the_original_build(self):
        middle = commit_files(self.repo, {"web/index.html": "2"}, "ui 1")
        self.assertEqual(self.reuse().returncode, 0)
        shutil.copytree(self.repo / "dist", self.checkouts / middle / "dist")
        shutil.rmtree(self.repo / "dist")
        manifest = self.checkouts / middle / ".treer" / "releases" / middle
        manifest.mkdir(parents=True)
        recorded = json.loads(subprocess.run(["sh", str(SCRIPTS / "machine-artifact-hashes.sh"), str(self.checkouts / middle / "dist")],
                                             check=True, capture_output=True, text=True).stdout)
        (manifest / "manifest.json").write_text(json.dumps({"status": "canary_passed", "commit": middle, "created_at": "2026-10-16T00:00:00Z",
                                                            "machine_artifacts": {"source_commit": self.source, "reused": True, "files": recorded}}))
        commit_files(self.repo, {"crates/core/lib.rs": "changed later"}, "native")
        self.assertNotEqual(self.reuse().returncode, 0)

    def test_source_without_recorded_hashes_is_never_reused(self):
        commit_files(self.repo, {"web/index.html": "2"}, "ui")
        self.install_source(self.source, with_hashes=False)
        self.assertNotEqual(self.reuse().returncode, 0)
        self.assertFalse((self.repo / "dist").exists())

    def test_binary_and_checksum_file_rewritten_together_do_not_match_the_recorded_hashes(self):
        commit_files(self.repo, {"web/index.html": "2"}, "ui")
        folder = self.checkouts / self.source / "dist" / "linux-x86_64"
        (folder / "treer").write_bytes(b"\x7fELFreplaced")
        sums = subprocess.run(["shasum", "-a", "256", *BINARIES], cwd=folder, check=True, capture_output=True, text=True).stdout
        (folder / "SHA256SUMS").write_text(sums)
        result = self.reuse()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("recorded at canary_passed", result.stderr)
        self.assertFalse((self.repo / "dist").exists())

    def hashes(self, directory):
        return subprocess.run(["sh", "scripts/machine-artifact-hashes.sh", str(directory)], cwd=self.repo, capture_output=True, text=True)

    def test_hash_helper_fails_closed_with_no_partial_output(self):
        dist = self.checkouts / self.source / "dist"
        self.assertEqual(len(json.loads(self.hashes(dist).stdout)), 25)
        (dist / "linux-x86_64" / "treer-agent-host").unlink()
        result = self.hashes(dist)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn("missing linux-x86_64/treer-agent-host", result.stderr)

    def test_hash_helper_fails_when_a_file_cannot_be_read_or_is_a_symlink(self):
        dist = self.checkouts / self.source / "dist"
        unreadable = dist / "darwin-aarch64" / "treer"
        unreadable.chmod(0)
        try:
            result = self.hashes(dist)
        finally:
            unreadable.chmod(0o755)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        link = dist / "windows-x86_64" / "SHA256SUMS"
        data = link.read_bytes()
        link.unlink()
        (dist / "real-sums").write_bytes(data)
        link.symlink_to(dist / "real-sums")
        result = self.hashes(dist)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")

    def test_provenance_without_the_full_hash_set_is_rejected(self):
        candidate = commit_files(self.repo, {"web/index.html": "2"}, "ui")
        self.assertEqual(self.reuse().returncode, 0)
        path = self.repo / "dist" / "artifact-provenance.json"
        provenance = json.loads(path.read_text())
        for broken in ({k: v for k, v in provenance.items() if k != "files"}, {**provenance, "files": {}}):
            path.write_text(json.dumps(broken))
            result = subprocess.run(["sh", "scripts/verify-machine-artifacts.sh", candidate, "dist"], cwd=self.repo, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("25 file hashes", result.stderr)

    def test_check_only_reports_reusability_without_creating_dist(self):
        commit_files(self.repo, {"web/index.html": "2"}, "ui")
        result = self.reuse(None, "--check-only")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("reusable", result.stdout)
        self.assertFalse((self.repo / "dist").exists())
        commit_files(self.repo, {"crates/core/lib.rs": "native"}, "native")
        self.assertNotEqual(self.reuse(None, "--check-only").returncode, 0)
        self.assertFalse((self.repo / "dist").exists())

    def verify_dist(self, directory="dist"):
        return subprocess.run(["sh", "scripts/reuse-machine-artifacts.sh", git(self.repo, "rev-parse", "HEAD"), "--verify-dist", directory],
                              cwd=self.repo, capture_output=True, text=True,
                              env={**os.environ, "TREER_ARTIFACT_REUSE_ROOT": str(self.checkouts)})

    def test_verify_dist_accepts_only_a_dist_matching_the_passed_releases_recorded_hashes(self):
        commit_files(self.repo, {"web/index.html": "2"}, "ui")
        self.assertEqual(self.reuse().returncode, 0)
        self.assertEqual(self.verify_dist().returncode, 0)
        manifest_path = self.checkouts / self.source / ".treer/releases" / self.source / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["machine_artifacts"]["files"]["linux-x86_64/treer"] = "0" * 64
        manifest_path.write_text(json.dumps(manifest))
        self.assertNotEqual(self.verify_dist().returncode, 0)
        manifest_path.unlink()
        self.assertNotEqual(self.verify_dist().returncode, 0)

    def test_verify_dist_rejects_a_dist_without_provenance_or_for_a_manifest_not_canary_passed(self):
        commit_files(self.repo, {"web/index.html": "2"}, "ui")
        self.assertEqual(self.reuse().returncode, 0)
        manifest_path = self.checkouts / self.source / ".treer/releases" / self.source / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["status"] = "failed"
        manifest_path.write_text(json.dumps(manifest))
        self.assertNotEqual(self.verify_dist().returncode, 0)
        (self.repo / "dist" / "artifact-provenance.json").unlink()
        self.assertNotEqual(self.verify_dist().returncode, 0)

    def image_dir(self):
        """A directory with no repository, like the hosted image's artifacts stage."""
        image = Path(self.temp.name) / "image"
        image.mkdir()
        for name in ("verify-machine-artifacts.sh", "machine-artifact-hashes.sh"):
            shutil.copy(SCRIPTS / name, image / name)
        return image

    def run_in_image(self, image, commit, directory, flag=True):
        env = {k: v for k, v in os.environ.items() if k != "TREER_ARTIFACT_VERIFY_NO_GIT"}
        if flag:
            env["TREER_ARTIFACT_VERIFY_NO_GIT"] = "1"
        return subprocess.run(["sh", str(image / "verify-machine-artifacts.sh"), commit, str(directory)],
                              cwd=image, capture_output=True, text=True, env=env)

    def test_hosted_image_verification_checks_bytes_without_git_but_only_when_asked(self):
        candidate = commit_files(self.repo, {"web/index.html": "2"}, "ui")
        self.assertEqual(self.reuse().returncode, 0)
        image = self.image_dir()
        ok = self.run_in_image(image, candidate, self.repo / "dist")
        self.assertEqual(ok.returncode, 0, ok.stderr)
        # Without the explicit image flag the git checks are mandatory and fail closed outside a repository.
        self.assertNotEqual(self.run_in_image(image, candidate, self.repo / "dist", flag=False).returncode, 0)

    def test_hosted_image_verification_still_rejects_tampering_and_foreign_provenance(self):
        candidate = commit_files(self.repo, {"web/index.html": "2"}, "ui")
        self.assertEqual(self.reuse().returncode, 0)
        image = self.image_dir()
        dist = self.repo / "dist"
        provenance = json.loads((dist / "artifact-provenance.json").read_text())
        # Another candidate commit in the provenance.
        (dist / "artifact-provenance.json").write_text(json.dumps({**provenance, "candidate_commit": "f" * 40}))
        self.assertNotEqual(self.run_in_image(image, candidate, dist).returncode, 0)
        (dist / "artifact-provenance.json").write_text(json.dumps(provenance))
        # Missing hashes.
        (dist / "artifact-provenance.json").write_text(json.dumps({k: v for k, v in provenance.items() if k != "files"}))
        self.assertNotEqual(self.run_in_image(image, candidate, dist).returncode, 0)
        (dist / "artifact-provenance.json").write_text(json.dumps(provenance))
        # Binary changed after the copy, checksum file rewritten to match.
        folder = dist / "linux-x86_64"
        (folder / "treer").write_bytes(b"\x7fELFswapped")
        sums = subprocess.run(["shasum", "-a", "256", *BINARIES], cwd=folder, check=True, capture_output=True, text=True).stdout
        (folder / "SHA256SUMS").write_text(sums)
        self.assertNotEqual(self.run_in_image(image, candidate, dist).returncode, 0)

    def test_dockerfile_hosted_gives_the_image_stage_the_helper_and_the_no_git_flag(self):
        text = (SCRIPTS.parent / "Dockerfile.hosted").read_text()
        self.assertIn("COPY scripts/verify-machine-artifacts.sh scripts/machine-artifact-hashes.sh /", text)
        self.assertIn("TREER_ARTIFACT_VERIFY_NO_GIT=1 sh /verify-machine-artifacts.sh", text)


if __name__ == "__main__":
    unittest.main()
