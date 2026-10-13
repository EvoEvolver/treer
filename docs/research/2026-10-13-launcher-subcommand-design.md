# Controller-owned default launcher installation

2026-10-13. Next release only; do not change candidate `dcb9d83`.

## Source and trust

Embed the default launcher payload in `treer-agent-server`, from the same build
checkout. Official release builds first produce a portable Node ESM bundle of
the runtime (including `ws`) from `package-lock.json`, plus the existing UI,
profiles and installer. A build-time file manifest records lengths and SHA256;
the payload content digest names its immutable installation directory. The
Controller build commit is recorded separately. The signed Controller artifact
authenticates the embedded bytes; installation verifies all bytes before running
the installer. There is no `latest`, Git fetch, independently fetched trust key,
or unsigned download fallback. Node 22+ and harness CLIs remain prerequisites.

This adds some binary size but avoids a second release/version/signature chain,
platform-specific `node_modules`, Git/npm on the target, and network installation.
The bundle does not vendor a Node executable. The operator can select a trusted
absolute `--node` path; automatic discovery is limited to known installation
locations, not arbitrary PATH entries. Node version is checked before execution
of the installer. Same-OS-account executable replacement remains outside Treer's
isolation guarantees. No identity tokens are put in requests or diagnostics.

For development, a Cargo build without a prepared bundle embeds source instead;
install fails clearly unless `--allow-network` explicitly enables lockfile-pinned
`npm ci --ignore-scripts`, using explicit Node/npm paths. This is not the official
offline path. Release builds must require a prepared runtime and fail if absent.
Any prebundling dependency lifecycle scripts run only in the trusted build job;
dependencies are lockfile/integrity checked. Asset hashes do not independently
authenticate an untrusted compiler, build host, or user-selected Node binary.

## Commands and shared installer contract

`treer-agent-server launcher install|update|status|rollback` is a local operation
against the selected enrolled machine. It never creates, starts, or restarts an
Agent/Host/Controller. `install` is idempotent; `update` selects this binary's
embedded payload, never a moving upstream ref. Both preserve all six existing
profile save/readback checks and transactional restoration. `rollback [VERSION]`
selects a retained, verified compatible version (previous by default), restores
profiles atomically where possible, and never deletes version assets. `status`
reports installed/current/previous versions, compatibility, and visible local
Agents' startup version references without printing their arguments or secrets.
Unreadable startup specifications are reported as unavailable, not guessed.

Core invokes the embedded `scripts/install.mjs --controller-request` with JSON
on stdin: `schema: treer.launcher-install/v1`, `operation`, `bundle_dir`, `version`
(`sha256-<64hex>`), `source_commit`, enrolled `root`, relative `cwd`, selected
`agents`, absolute `node`, `treer`, optional `npm_cli`, `allow_network`, and
`controller: {git_commit, protocol, capabilities, features}`. Status/rollback
do not need a candidate; rollback can supply `rollback_version`. Output is JSON;
errors are sanitized rather than echoing subprocess output/identity responses.
The launcher owner maintains this transaction engine; Core owns extraction,
entry-point trust checks, Controller feature reporting, and startup inspection.

## Layout, compatibility and failure behavior

Use the enrolled physical machine root's private `.treer/launchers/agent` tree.
Check every managed component without following symlinks/reparse points, enforce
0700 directories/0600 state on Unix and the existing private-user ACL boundary
on Windows. A shared install lock covers profiles, current pointer and rollback;
do not auto-break a live/stale lock by PID guess. Publish an atomic JSON current
pointer with a stable Node trampoline (Windows requires no symlink privilege).
Each running launcher resolves one immutable physical version once. Legacy
`current` symlinks are inspected only as explicit migration pointers, never as
extraction destinations. Failed profile restoration is loud and keeps a usable
candidate rather than leaving profiles pointing at deleted assets.

A shared launcher interface contract supplies registration and manifest
capabilities. Controller advertises a versioned launcher-install feature and
supported AIS/provenance/queue features. Installation and runtime registration
check the actual selected Controller, not just the installer binary's version;
missing features, unknown contract or protocol disagreement fail explicitly.
Status distinguishes compatible, incompatible and unknown, never treats missing
metadata as compatible. Rollback cannot bypass these checks. Existing running
launchers are untouched; a new Controller may continue supporting an older
declared contract. Old Controller/new launcher fails closed before registration.
An old binary lacking this subcommand reports an ordinary unknown command.

`install.sh`/`install.mjs`/`bootstrap.sh` remain documented legacy/development
entry points delegating to the same transaction engine where applicable;
`switch.sh` remains a separate explicit Agent restart action. New profiles use
the cross-platform Node entry point; shell wrappers remain compatible.

## Delivery and verification

1. Core commits this contract; launcher-dev implements the shared installer,
   portable payload builder, Node entry and compatibility contract. Core adds
   embedded file manifest/extraction, CLI, local feature handshake and tests.
2. release-eng integrates payload preparation into platform/release builds,
   rejecting source-only official artifacts; reviews hashes and installation.
3. Cross-review Core/launcher, release-eng supply chain and security-claude
   adversarial review before local-main merge. No deployment or runtime restart.

Tests cover corrupt bytes and traversal/link rejection before execution, Node
minimum version/untrusted discovery, offline installation, six profile readbacks,
idempotence, concurrency, rollback after each failed mutation, immutable running
assets, capability mismatch in both version directions, and status startup
reporting. Run isolated fake Treer tests without real credentials or running
Agents; Linux/macOS tests plus Windows native/compilation checks are recorded
honestly. This is a medium change split across Core, launcher and release owners;
it does not move launcher lifecycle or harness credentials into Core.
