from __future__ import annotations

import json
import threading
import time
import unittest
from pathlib import Path
import sys


APP_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(APP_ROOT))

from updater import (  # noqa: E402
    Updater,
    UpdaterError,
    display_digest,
    first_repo_digest,
    require_channel,
    running_image_refs,
    service_record,
)


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

    def test_container_inspect_without_repo_digests_uses_the_image(self) -> None:
        container = {
            "Image": "sha256:old",
            "ImageManifestDescriptor": {"digest": "sha256:platform"},
            "Config": {"Image": "ghcr.io/evoevolver/treer-proxy:stable", "Labels": {}},
        }
        image = {
            "Id": "sha256:old",
            "RepoDigests": ["ghcr.io/evoevolver/treer-proxy@sha256:index"],
        }
        refs = running_image_refs(container, image)
        self.assertIn("sha256:old", refs)
        self.assertIn("sha256:platform", refs)
        self.assertIn("sha256:index", refs)
        self.assertEqual(display_digest(container, image), "sha256:index")
        record = service_record("proxy", container, image)
        self.assertEqual(record["digest"], "sha256:index")

    def test_marks_missing_containers(self) -> None:
        self.assertEqual(service_record("proxy", None)["present"], False)


class UpdaterTests(unittest.TestCase):
    def setUp(self) -> None:
        self.commands: list[list[str]] = []
        self.digests = {
            "ghcr.io/evoevolver/treer-proxy": {"sha256:new-proxy", "sha256:new-proxy-platform"},
            "ghcr.io/evoevolver/treer-app": {"sha256:new-app"},
            "ghcr.io/evoevolver/treer-updater": {"sha256:old-updater"},
        }
        self.containers = {
            "proxy": {
                "Image": "sha256:old-proxy",
                "Config": {
                    "Image": "ghcr.io/evoevolver/treer-proxy:stable",
                    "Labels": {
                        "org.opencontainers.image.version": "0.1.2",
                        "org.opencontainers.image.revision": "abc",
                    },
                },
            },
            "app": {
                "Image": "sha256:old-app",
                "Config": {
                    "Image": "ghcr.io/evoevolver/treer-app:stable",
                    "Labels": {"org.opencontainers.image.version": "0.1.2"},
                },
            },
            "updater": {
                "Image": "sha256:old-updater",
                "Config": {"Image": "ghcr.io/evoevolver/treer-updater:stable", "Labels": {}},
            },
        }
        self.images = {
            "sha256:old-proxy": {
                "Id": "sha256:old-proxy",
                "RepoDigests": ["ghcr.io/evoevolver/treer-proxy@sha256:old-proxy"],
            },
            "sha256:old-app": {
                "Id": "sha256:old-app",
                "RepoDigests": ["ghcr.io/evoevolver/treer-app@sha256:old-app"],
            },
            "sha256:old-updater": {
                "Id": "sha256:old-updater",
                "RepoDigests": ["ghcr.io/evoevolver/treer-updater@sha256:old-updater"],
            },
            "ghcr.io/evoevolver/treer-proxy:stable": {
                "Id": "sha256:new-proxy",
                "RepoDigests": ["ghcr.io/evoevolver/treer-proxy@sha256:new-proxy"],
            },
            "ghcr.io/evoevolver/treer-app:stable": {
                "Id": "sha256:new-app",
                "RepoDigests": ["ghcr.io/evoevolver/treer-app@sha256:new-app"],
            },
            "ghcr.io/evoevolver/treer-updater:stable": {
                "Id": "sha256:old-updater",
                "RepoDigests": ["ghcr.io/evoevolver/treer-updater@sha256:old-updater"],
            },
        }

        def run_docker(args: list[str]) -> str:
            self.commands.append(args)
            if args[:1] == ["compose"] and "ps" in args:
                return f"{args[-1]}-container\n"
            if args[:1] == ["inspect"]:
                target = args[1]
                if target.endswith("-container"):
                    return json.dumps([self.containers[target.removesuffix("-container")]])
                if target in self.images:
                    return json.dumps([self.images[target]])
                if target in self.containers:
                    return json.dumps([self.containers[target]])
                raise AssertionError(args)
            if args[:1] == ["pull"]:
                return ""
            if args[:1] == ["rm"]:
                return ""
            if args[:1] == ["run"]:
                return "helper\n"
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

    def _wait_for_job(self) -> None:
        deadline = time.time() + 2
        while time.time() < deadline:
            job = self.updater.status()["job"]
            if job and job["state"] != "running":
                return
            time.sleep(0.01)

    def test_check_reports_a_newer_channel_digest(self) -> None:
        report = self.updater.check()
        self.assertTrue(report["update_available"])
        proxy = next(item for item in report["services"] if item["name"] == "proxy")
        self.assertEqual(proxy["digest"], "sha256:old-proxy")
        self.assertEqual(proxy["channel_digest"], "sha256:new-proxy")
        self.assertTrue(proxy["update_available"])
        updater = next(item for item in report["services"] if item["name"] == "updater")
        self.assertFalse(updater["update_available"])
        self.assertEqual(updater["digest"], "sha256:old-updater")

    def test_check_treats_index_and_platform_digests_as_current(self) -> None:
        self.containers["proxy"]["ImageManifestDescriptor"] = {"digest": "sha256:new-proxy-platform"}
        self.digests["ghcr.io/evoevolver/treer-proxy"] = {
            "sha256:new-proxy",
            "sha256:new-proxy-platform",
        }
        self.images["sha256:old-proxy"]["RepoDigests"] = [
            "ghcr.io/evoevolver/treer-proxy@sha256:new-proxy"
        ]
        self.containers["proxy"]["Image"] = "sha256:new-proxy"
        self.images["sha256:new-proxy"] = {
            "Id": "sha256:new-proxy",
            "RepoDigests": ["ghcr.io/evoevolver/treer-proxy@sha256:new-proxy"],
        }
        self.images["ghcr.io/evoevolver/treer-proxy:stable"]["Id"] = "sha256:new-proxy"
        report = self.updater.check()
        proxy = next(item for item in report["services"] if item["name"] == "proxy")
        self.assertFalse(proxy["update_available"])

    def test_check_does_not_offer_apply_when_running_digest_is_unknown(self) -> None:
        self.containers["proxy"] = {"Config": {"Image": "ghcr.io/evoevolver/treer-proxy:stable", "Labels": {}}}
        report = self.updater.check()
        proxy = next(item for item in report["services"] if item["name"] == "proxy")
        self.assertIsNone(proxy["digest"])
        self.assertFalse(proxy["update_available"])

    def test_apply_pulls_channel_tags_then_recreates_only_changed_services(self) -> None:
        result = self.updater.apply()
        self.assertIn(result["job"]["state"], {"running", "succeeded"})
        self._wait_for_job()
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
            ups,
            [
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
                ]
            ],
        )
        self.assertFalse(any(args[:1] == ["run"] for args in self.commands))

    def test_apply_recreates_updater_in_a_detached_helper(self) -> None:
        self.digests["ghcr.io/evoevolver/treer-updater"] = {"sha256:new-updater"}
        self.images["ghcr.io/evoevolver/treer-updater:stable"] = {
            "Id": "sha256:new-updater",
            "RepoDigests": ["ghcr.io/evoevolver/treer-updater@sha256:new-updater"],
        }
        self.updater.apply()
        self._wait_for_job()
        self.assertEqual(self.updater.status()["job"]["state"], "succeeded")
        runs = [args for args in self.commands if args[:1] == ["run"]]
        self.assertEqual(len(runs), 1)
        self.assertIn("treer-updater-recreate", runs[0])
        self.assertIn("--entrypoint", runs[0])
        self.assertEqual(runs[0][-1], "updater")
        self.assertNotIn(
            "updater",
            [args[-1] for args in self.commands if args[:1] == ["compose"] and "up" in args],
        )

    def test_apply_rejects_when_already_current(self) -> None:
        for name in ("proxy", "app"):
            digest = f"sha256:old-{name}"
            self.digests[f"ghcr.io/evoevolver/treer-{name}"] = {digest}
        with self.assertRaises(UpdaterError) as raised:
            self.updater.apply()
        self.assertEqual(raised.exception.code, "already_current")

    def test_apply_rejects_a_second_running_job(self) -> None:
        started = threading.Event()
        release = threading.Event()

        def run_docker(args: list[str]) -> str:
            if args[:1] == ["compose"] and "ps" in args:
                return f"{args[-1]}-container\n"
            if args[:1] == ["inspect"]:
                target = args[1]
                if target.endswith("-container"):
                    return json.dumps([self.containers[target.removesuffix("-container")]])
                if target in self.images:
                    return json.dumps([self.images[target]])
                raise AssertionError(args)
            if args[:1] == ["pull"]:
                started.set()
                release.wait(timeout=2)
                return ""
            if args[:1] == ["compose"] and "up" in args:
                return ""
            raise AssertionError(args)

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
            registry_digest=lambda image, tag: self.digests[image],
        )
        updater.apply()
        self.assertTrue(started.wait(timeout=2))
        with self.assertRaises(UpdaterError) as raised:
            updater.apply()
        self.assertEqual(raised.exception.code, "update_in_progress")
        release.set()


if __name__ == "__main__":
    unittest.main()
