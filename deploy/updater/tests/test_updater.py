from __future__ import annotations

import json
import threading
import time
import unittest
from pathlib import Path
import sys


APP_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(APP_ROOT))

from updater import Updater, UpdaterError, first_repo_digest, require_channel, service_record  # noqa: E402


class ChannelTests(unittest.TestCase):
    def test_rejects_latest(self) -> None:
        with self.assertRaises(UpdaterError) as raised:
            require_channel("latest")
        self.assertEqual(raised.exception.code, "invalid_channel")


class ImageTests(unittest.TestCase):
    def test_rejects_non_ghcr_images(self) -> None:
        from updater import require_image

        with self.assertRaises(UpdaterError) as raised:
            require_image("docker.io/library/nginx")
        self.assertEqual(raised.exception.code, "invalid_image")


class DigestTests(unittest.TestCase):
    def test_reads_the_first_repo_digest(self) -> None:
        self.assertEqual(
            first_repo_digest(
                {"RepoDigests": ["ghcr.io/evoevolver/treer-proxy@sha256:abc"]}
            ),
            "sha256:abc",
        )

    def test_marks_missing_containers(self) -> None:
        self.assertEqual(service_record("proxy", None)["present"], False)


class UpdaterTests(unittest.TestCase):
    def setUp(self) -> None:
        self.commands: list[list[str]] = []
        self.digests = {
            "ghcr.io/evoevolver/treer-proxy": "sha256:new",
            "ghcr.io/evoevolver/treer-app": "sha256:new",
            "ghcr.io/evoevolver/treer-updater": "sha256:old",
        }
        self.inspect = {
            "proxy": {
                "RepoDigests": ["ghcr.io/evoevolver/treer-proxy@sha256:old"],
                "Config": {
                    "Image": "ghcr.io/evoevolver/treer-proxy:stable",
                    "Labels": {
                        "org.opencontainers.image.version": "0.1.2",
                        "org.opencontainers.image.revision": "abc",
                    },
                },
            },
            "app": {
                "RepoDigests": ["ghcr.io/evoevolver/treer-app@sha256:old"],
                "Config": {
                    "Image": "ghcr.io/evoevolver/treer-app:stable",
                    "Labels": {"org.opencontainers.image.version": "0.1.2"},
                },
            },
            "updater": {
                "RepoDigests": ["ghcr.io/evoevolver/treer-updater@sha256:old"],
                "Config": {"Image": "ghcr.io/evoevolver/treer-updater:stable", "Labels": {}},
            },
        }

        def run_docker(args: list[str]) -> str:
            self.commands.append(args)
            if args[:1] == ["compose"] and "ps" in args:
                name = args[-1]
                return f"{name}-container\n"
            if args[:1] == ["inspect"]:
                name = args[1].removesuffix("-container")
                return json.dumps([self.inspect[name]])
            if args[:1] == ["pull"]:
                return ""
            if args[:1] == ["compose"] and "up" in args:
                return ""
            raise AssertionError(args)

        self.updater = Updater(
            token="secret",
            channel="stable",
            images={
                "proxy": "ghcr.io/evoevolver/treer-proxy",
                "app": "ghcr.io/evoevolver/treer-app",
                "updater": "ghcr.io/evoevolver/treer-updater",
            },
            compose_file="/compose/compose.yaml",
            compose_project="treer",
            run_docker=run_docker,
            registry_digest=lambda image, _tag: self.digests[image],
        )

    def test_check_reports_a_newer_channel_digest(self) -> None:
        report = self.updater.check()
        self.assertTrue(report["update_available"])
        proxy = next(item for item in report["services"] if item["name"] == "proxy")
        self.assertEqual(proxy["digest"], "sha256:old")
        self.assertEqual(proxy["channel_digest"], "sha256:new")
        self.assertTrue(proxy["update_available"])
        updater = next(item for item in report["services"] if item["name"] == "updater")
        self.assertFalse(updater["update_available"])

    def test_apply_pulls_channel_tags_then_recreates_proxy_and_app(self) -> None:
        result = self.updater.apply()
        self.assertIn(result["job"]["state"], {"running", "succeeded"})
        deadline = time.time() + 2
        while time.time() < deadline:
            job = self.updater.status()["job"]
            if job and job["state"] != "running":
                break
            time.sleep(0.01)
        self.assertEqual(self.updater.status()["job"]["state"], "succeeded")
        pulls = [args for args in self.commands if args[:1] == ["pull"]]
        self.assertEqual(
            pulls,
            [
                ["pull", "ghcr.io/evoevolver/treer-proxy:stable"],
                ["pull", "ghcr.io/evoevolver/treer-app:stable"],
                ["pull", "ghcr.io/evoevolver/treer-updater:stable"],
            ],
        )
        ups = [args for args in self.commands if args[:1] == ["compose"] and "up" in args]
        self.assertEqual(
            ups[0],
            [
                "compose",
                "-f",
                "/compose/compose.yaml",
                "-p",
                "treer",
                "up",
                "-d",
                "--no-deps",
                "--pull",
                "never",
                "proxy",
                "app",
            ],
        )
        self.assertEqual(ups[1][-1], "updater")

    def test_apply_rejects_a_second_running_job(self) -> None:
        started = threading.Event()
        release = threading.Event()

        def run_docker(args: list[str]) -> str:
            if args[:1] == ["pull"]:
                started.set()
                release.wait(timeout=2)
            return ""

        updater = Updater(
            token="secret",
            channel="stable",
            images={
                "proxy": "ghcr.io/evoevolver/treer-proxy",
                "app": "ghcr.io/evoevolver/treer-app",
                "updater": "ghcr.io/evoevolver/treer-updater",
            },
            compose_file="/compose/compose.yaml",
            compose_project="treer",
            run_docker=run_docker,
            registry_digest=lambda image, tag: "sha256:new",
        )
        updater.apply()
        self.assertTrue(started.wait(timeout=2))
        with self.assertRaises(UpdaterError) as raised:
            updater.apply()
        self.assertEqual(raised.exception.code, "update_in_progress")
        release.set()


if __name__ == "__main__":
    unittest.main()
