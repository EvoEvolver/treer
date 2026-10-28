# Release process

- Status: maintained
- Last reviewed: 2026-10-27

A release is an exact Git commit on `main`. The same commit is built once per
platform by CI, deployed to the hosted Canary, then to Production, tagged for
self-host images, and optionally signed into the R2 update channels.

```bash
just check                    # everything CI runs, locally (needs `just test-db-up`)
just deploy canary            # hosted Canary: Railway Proxy + Cloudflare App
just test-canary              # two-machine black-box test against Canary
just deploy production        # same clean commit, must be in origin/main
git tag vX.Y.Z && git push origin vX.Y.Z   # GHCR self-host images + tag artifacts
just artifacts-prepare vX.Y.Z && just artifacts-canary vX.Y.Z   # signed R2 channel
just artifacts-stable vX.Y.Z
```

## Hosted platform

Railway runs the Proxy replicas, PostgreSQL, NATS and the two Canary test
machines. Cloudflare Workers Static Assets serves the React App; only `/health`
and `/config.json` run Worker code, so one bundle works for any Proxy origin.

| Environment | App | Proxy |
| --- | --- | --- |
| Canary | `https://app.canary.treer.ai/` | `https://proxy.canary.treer.ai/` |
| Production | `https://app.treer.ai/` | `https://proxy.treer.ai/` |

`scripts/deploy-hosted.sh canary|production` (`just deploy`) requires a clean
worktree and authenticated `railway`, `wrangler` (through `pnpm`), `gh`, `jq`
and `curl`. It:

1. puts verified native machine artifacts for `HEAD` into `dist/`
   (`scripts/fetch-machine-artifacts.sh`, see below);
2. uploads `git archive HEAD` plus `dist/` to Railway with
   `railway.json` rewritten to `Dockerfile.hosted`, which bundles all fifteen
   machine binaries without recompiling them and re-verifies them in the image;
3. builds `web/` and runs `wrangler deploy --env <target>`;
4. waits for the Railway deployment, then checks Proxy health, every served
   `/artifacts/<platform>/<binary>` byte for byte without redirects
   (`verify-served-artifacts.sh`), App `/health` and `/config.json`, and the
   served App HTML against the local build (`verify-served-frontend.py`);
5. on Canary checks the wildcard ingress certificate; on Production the wildcard
   404 probe. Production also refuses a commit not contained in `origin/main`.

Railway IDs and URLs default to the Treer project; override with
`TREER_RAILWAY_PROJECT_ID`, `TREER_PROXY_SERVICE`, `TREER_RAILWAY_ENVIRONMENT`,
`TREER_PROXY_URL`, `TREER_APP_URL`, `TREER_INGRESS_URL`,
`TREER_WORKER_ENVIRONMENT` and `TREER_DEPLOY_TIMEOUT`. `just test-canary` and its
fixtures are described in [Canary environment](canary.md).

Optional Cloudflare Workers Builds may upload inactive Canary App versions from
`main` (`worker:upload:canary`); uploads never receive traffic.

Roll back the App with `wrangler rollback --env production <version-id>` and the
Proxy by redeploying its previous Railway deployment. Database contraction is
never part of a code rollback: use expand-first PostgreSQL changes so old and
new Proxy replicas can overlap, and add Proxy API fields before the App uses them.
A Controller whose startup store is v2 (reservation fences) cannot roll back to a
Controller without `agent.reservation-fence.v1`; never delete fences to force it.
Back up App state (Mail, Telegram) before replacing an App; stop App processes,
not Core Message routes, to stop channel traffic.

## Machine artifacts

`Build release artifacts` (`.github/workflows/build-release-artifacts.yml`) runs
for `v*` tags or by manual dispatch with a `revision`. Native runners build the
CLI, Host and Controller for `linux-x86_64`, `linux-aarch64`, `darwin-x86_64`,
`darwin-aarch64` and `windows-x86_64`, after preparing the launcher payload with
`scripts/prepare-launcher-release.mjs` (lockfile-pinned `npm ci --ignore-scripts`;
the build fails without a valid payload). `scripts/stage-release-artifacts.sh`
(`.ps1` on Windows) stages each platform with `SHA256SUMS` and
`build-metadata.json`. Windows also runs ConPTY/Controller/CLI tests, the
standard-user and installer checks, a byte-identical clean rebuild (`/Brepro`)
and the native embedded-launcher smoke. The workflow never publishes anything.

`scripts/fetch-machine-artifacts.sh [revision]` (`just fetch-artifacts`) verifies
an existing `dist/`, or downloads the newest successful run for the exact commit
(`TREER_ARTIFACT_RUN_ID` selects one), dispatching the workflow if no run exists.
It requires the Windows launcher job to have passed and rejects missing
platforms, mismatched commit metadata, partial checksum files and missing
executable bits (`scripts/verify-machine-artifacts.sh`). A stale `dist/` fails;
delete it to fetch another commit. `just stage-artifacts` builds only the local
platform for testing.

Windows artifacts keep the logical names without `.exe`; `scripts/install.ps1`
saves them with the suffix and fails closed without `SHA256SUMS`.

## Self-host images

Pushing a `vX.Y.Z` tag that matches `Cargo.toml` runs `Publish GHCR images`:
native `linux/amd64` and `linux/arm64` images for `treer-proxy` (`Dockerfile`),
`treer-app` (`web/Dockerfile`) and `treer-updater` (`deploy/updater/Dockerfile`)
with immutable `vX.Y.Z` and `sha-<commit>` tags and a moving `canary` pointer.
After Production, dispatch the workflow with `channel=stable` and the version;
`scripts/retag-ghcr.sh` retags existing digests without rebuilding. `latest` is
never published. Compose pulls these images; see [Self-hosted Treer](../deploy/README.md).

The same artifact workflow also stages `treer-proxy` for both Linux platforms
(`stage-release-artifacts.sh --proxy`), assembles `.single-container-input/`
with `scripts/assemble-single-container-input.py`, builds `Dockerfile.single`
from that exact input digest on each architecture and runs
`deploy/single-container/smoke.mjs`. That image is not pushed; operators build
it locally with `scripts/build-single-image.sh`.

## Signed R2 releases

Installed Controllers update only from the signed R2 channel. `scripts/release-r2.mjs`
prepares a version manifest from `dist/`, signs it, publishes it to the `canary`
channel and promotes it to `stable` (`just artifacts-*`). Both signatures, the
channel's manifest SHA256 and every binary's signed size and SHA256 must verify
before downloaded code runs. See
[Publish signed R2 releases](operator-guide.md#publish-signed-r2-releases).

## Official release signing authority

The public SPKI in `keys/official-release-ed25519.pub.pem` is the authority for
`https://releases.treer.ai/`; its DER SHA256 is
`634761ff47a6a113b4b8925f0db0e030d8bfce07f2fa228fdc0a4904874dc41f` and
`keys/official-release-authority.json` records its designation. New Controllers
embed the same key.

The private key lives in the publisher's macOS login Keychain as a
non-synchronizing item with an explicit signer-executable ACL; it never exists as
a PEM file, argv, Git object or stdout. Build `scripts/release-keychain.swift`
with `xcrun swiftc -O` into a stable owner-only directory (mode 700) and record
its SHA256. `status`, `public`, `sign` (≤ 1 MiB stdin, detached signature) and
`selftest` are routine; `create` establishes the authority once and refuses an
existing item. There is no export. Pass `--keychain-signer ABS --signer-sha256
HASH` to `release-r2.mjs prepare|publish|promote`; it pins the executable, checks
its key against the repository SPKI and verifies every returned signature
(`scripts/release-keychain.mjs`). Self-hosted publishers may use an explicit key
file instead, never together with Keychain custody.

For an existing installation without the key,
`node scripts/install-release-trust.mjs [--config-dir ABS]` installs only the
shipped public key (exclusive create; refuses a different or unsafe existing key).

## Launcher channel

`treer-agent update --latest` reads the launcher-only pointer
`launcher-stable/launcher-latest.json` on GitHub Releases; version objects live
under `launcher-<full-source-SHA>` and are never overwritten. Stage with
`node scripts/launcher-release.mjs stage --payload ABS --native ABS --binding ABS
--build ABS --source FULL_SHA --platform PLATFORM --output ABS` and publish once
with `node scripts/launcher-release.mjs publish ABS_OUTPUT ABS_STAGE_JSON...`.
Trust is the repository publisher over HTTPS plus pinned SHA256.
