# Manual machine rollout

`machine-rollout.mjs` automates an authorized leader's machine pass. It does not
publish a release, update itself, restart a Host, or restart existing Agents.
Node.js 22+ is required locally and on every target; POSIX targets also need a
trusted absolute Python 3.11+ interpreter. Windows uses its built-in Windows
PowerShell 5.1 and CIM. Provider CLIs and their login remain user prerequisites.

Run it from a trusted repository checkout, using the script at its original
`scripts/machine-rollout.mjs` location. Do not copy just that file elsewhere: it
imports the worker and `launchers/agent/scripts/private-state.mjs`, and uploads
the checked-out updater/helper files. Missing checkout files fail before a job
can be submitted (for example, `ERR_MODULE_NOT_FOUND`).

First inspect an offline plan (no network, writes, or machine commands):

```sh
node scripts/machine-rollout.mjs --sha FULL_SHA \
  --manifest .treer/releases/FULL_SHA/manifest.json \
  --targets /absolute/private/targets.json --proxy https://proxy.canary.treer.ai
```

Then add `--apply --receipt /absolute/private/new-job/receipt.json`. The receipt
parent must be a **new** directory under an existing trusted parent; it is made
private, without changing permissions on an existing user directory. File mode
is 0600, directory mode 0700 (current-user/SYSTEM-only DACL on Windows). Use
`--treer /absolute/trusted/treer` to select the coordinating CLI explicitly.

The full 40-character SHA must match a local `canary_passed` release manifest.
For every selected platform the manifest must pin all three binaries and the
exact `SHA256SUMS` bytes. Downloaded checksums must match those independent pins;
changed Canary artifacts fail closed rather than silently upgrading to latest.
The manifest's `machine_artifacts.source_commit` identifies binary versions,
including releases that deliberately reused verified machine artifacts. The
operator must trust the local release receipt and explicitly supplied HTTPS
Proxy origin: checksum pinning does **not** add independent publisher signatures.
Redirects, credentials in URLs and ambient download proxies are refused.
The operator needs Policy authorization for `machine.exec`, `machine.file.write`
(including the private job directory), and Agent create/delete. Keep targets and
receipts outside the repository and shared directories: they reveal installation
and backup locations. Treat backups as sensitive startup intent.

## Explicit targets

`targets.json` is an array, in rollout order. Discover real IDs and paths first;
never put credentials, provider configuration or startup argv in this file.
Unknown fields are rejected. Example for a POSIX machine:

```json
[
  {
    "machine": "build-box",
    "serverId": "srv_example",
    "workspace": "ws_example",
    "platform": "linux-x86_64",
    "home": "/home/operator",
    "node": "/absolute/trusted/node",
    "python": "/absolute/trusted/python3",
    "controller": "/home/operator/.local/libexec/treer/treer-agent-server",
    "cli": "/home/operator/.local/bin/treer",
    "config": "/home/operator/.config/treer/agent-servers/CONFIG_KEY-controller.json",
    "startupStore": "/home/operator/.local/state/treer/srv_example-agent-startup.json",
    "harness": "claude"
  }
]
```

Platforms: `darwin-aarch64`, `darwin-x86_64`, `linux-aarch64`, `linux-x86_64`,
`windows-x86_64`. For Windows, use absolute Windows paths, omit `python`, and use
the same binary directory for `controller` and `cli`. Config normally lives in
`%LOCALAPPDATA%\Treer\agent-servers`, startup state in
`%LOCALAPPDATA%\Treer\state`. All replacement/config/state paths must be physical
paths without symlink/junction ancestors. Select the actual installed files, not
aliases; unrelated copies/aliases are left unchanged. `startupStore` must be an
existing file named `<serverId>-agent-startup.json`. Backups contain the original
startup intent and can be sensitive; never commit or paste their contents.

Optional `configHome` sets the target POSIX XDG config base. Optional
`acceptSharedBinaries: true` explicitly acknowledges a same-user, multi-workspace
installation: shared binaries change for other workspaces' **next** start, while
only the selected Controller is restarted. Without that acknowledgement the
existing updaters refuse shared installs. Do not use it without coordinating the
other owners. Windows currently requires the default local config directory and
detached service manager, as the existing Windows updater does.
Windows PS5.1's file APIs also have legacy path limits: preflight refuses paths
whose generated backup or retained-image names can reach 260 characters. Use a
shorter machine root rather than treating verbatim prefixes as a universal fix.

`harness` is explicitly `codex`, `claude`, or `acp`; for ACP also supply an absolute
trusted `acpCommand`. No automatic login, harness fallback or credentials copying
occurs. Optional `smokeFolder` is an authorized absolute Tree folder (default `/`),
independent of the smoke Agent's filesystem cwd (a new empty `smoke` directory
inside its private rollout job, passed as a path relative to the physical machine
root as required by the Host).

## Execution and recovery

Machines run sequentially, stopping on the first failure/partial/pending result.
Before creating any job directory, the coordinator uploads a harmless small
probe to a unique `rollout-probe-<uuid>.txt` in the machine root (mode 0600), then
verifies and deletes that exact file through exec. Transfer denial fails clearly
before setup; it is not retried through exec/base64 or a local authorization
bypass. If probe verification/cleanup fails, inspect that exact probe name in
the transfer audit before retrying. No binary or startup state has changed.

The coordinator uploads only repo-owned worker code into a new private
`<machine.root>/treer-rollouts/<sha>-<uuid>/` directory. The `.treer` namespace is
reserved Core state and cannot be a machine-file-transfer destination. It hashes the
uploaded worker files before executing them. POSIX uses an independent process
session; Windows uses CIM `Win32_Process.Create`, so the worker survives the
Controller exec timeout/restart. No broad process-name killing is used.
Local machines use the same transfer checks; they are not guessed from machine
names or silently changed to direct filesystem writes. A Controller with a
poisoned legacy transfer journal must first receive the Core recovery fix through
an authorized update; this script neither edits that journal nor changes Policy.

The platform updaters are reused, with a binary lock, checksummed staging and
private backups of installed binaries plus the selected startup store. They
replace the Controller first, invoke `service --workspace … restart-controller`
with a clean environment, wait for a fresh matching Controller epoch/reconnection
and unchanged Agent PID/generation inventory, then replace the CLI. Host bytes and
processes are preserved. Windows renames loaded images to `.prev-<commit>-<uuid>`
instead of overwriting them. POSIX CLI and Controller may be in separate physical
directories. Existing updater failure recovery restores changed binaries; it
does not blindly restore a live startup store over newer user intent.

After core activation, the worker explicitly binds workspace/server/loopback URL
for `launcher install` and all CLI calls. It creates one UUID-named smoke Agent,
waits for `idle` **and** a registered `treer.agent-interface/v1` interface, then
deletes only the Agent ID returned by its own creation. This checks startup and
UI registration, not an authenticated model prompt/reply.
The dedicated empty cwd avoids project settings in the enrolled root itself;
it does not disable provider-global or inherited ancestor hooks/MCP settings.
Existing Agents are
not switched to a new launcher or restarted. Installation saves the canonical
shared profiles in the selected workspace.

If launcher installation or smoke fails after core activation, the receipt says
`partial`; it does not claim that profiles, provider effects or a core update
were rolled back. Inspect the retained private core receipt/backup and the smoke
ID/name, repair explicitly, and only then make a fresh plan for remaining
machines. An uncertain create reply is never “cleaned up” by deleting some
matching Agent name. A known smoke ID is recorded before polling/deletion; a
failed delete remains inspectable in `progress.json`.
A core subprocess failure without a terminal transaction receipt is also
`partial` with `coreStatus: "unconfirmed"`: activation or rollback may have been
interrupted and must be inspected before another attempt.

The local receipt pointer is saved **before** the start RPC. If its reply or the
connection is lost, do not reapply; poll that same job:

```sh
node scripts/machine-rollout.mjs --resume /absolute/private/new-job/receipt.json
```

Resume polls submitted jobs only; it does not retry mutations or automatically
advance an incomplete train. A poll deadline returns `pending`; the independent
worker may still be running. Keep the private job, inspect its recorded PID and
progress, and resume. Never remove an updater lock merely because a command
timed out. Receipts contain phases/status/backup paths/IDs, not subprocess logs,
credentials, startup contents or environment dumps. They do not prove isolation
from another process running as the same OS user.

## Closed checks

An unconfirmed probe cleanup records its exact root-relative filename in the
private receipt's `preflightFailure.probe` and stderr. Inspect the transfer audit
before removing that specific leftover file; no rollout setup has occurred.
The visible `treer-rollouts` directory contains operational data including machine
paths and hashes. Exclude it from root-level indexing/version control, or remove
the specific completed job directory after confirming its receipt and that its
worker has exited. Never delete a pending job or use broad prefix cleanup.

```sh
node --test scripts/machine-rollout.test.mjs
python3 -m unittest discover -s scripts/tests -p test_update_machine.py
```

Native Windows also runs `scripts/test-windows-update.ps1 -ArtifactDirectory …`
against a private fixture with copied official version-only executables. Run it
outside a Controller Job (via an owned CIM test runner); direct exec is bounded
to 30 seconds. These tests must not activate a real enrollment. Compile/static
checks and fake platform execution are not substitutes for native Windows
PowerShell/CIM evidence or an operator-authorized real machine rollout.

The Windows native gate calls the real updater inventory helper with Controller
responses that omit optional `pid`, as well as explicit null and running PIDs.
No-process agents remain in the preservation snapshot; Agent removal, PID or
run-generation changes still fail preservation. Required identity fields remain
strict. This covers a pre-activation failure hidden by transaction mocks that
previously replaced the inventory helper with a constant.

The native gate also writes the real progress receipt through running,
activating and complete, checks its DACL survives replacement, and rejects
temporary-file leftovers. Windows PowerShell 5.1 needs `[NullString]::Value`
for File.Replace's optional backup path; `$null` becomes an invalid empty path.
Depending on the parent DACL, Windows may merge an explicit duplicate of an
inherited access rule during replacement. The gate tolerates only identical
duplicates: principals, rights, allow/deny, propagation, protected status and
the continued presence of inherited rules and the owner are still checked. Negative controls
verify that new principals, masks, deny rules, propagation or lost inherited
rules, ownership or protection changes fail comparison. Failure diagnostics use fixed codes and numeric rules
with principals labeled current/system/other, never account names or SIDs.
These regression fixtures do not replace disposable-enrollment acceptance:
the real updater must finish activation and launcher smoke against an isolated
Controller before retrying an existing user's machine after a newly found
transaction failure.

Probe uploads canonicalize the local temporary directory before creating their
0600 source file. This supports macOS `/var` and `/tmp` aliases without relaxing
the CLI's no-follow local-file boundary.

On Windows the activation PowerShell process itself starts through CIM with
`CREATE_BREAKAWAY_FROM_JOB`; a CIM-started Node process can still place its child
processes in a Job. The updater's `Assert-UpdateOutsideJob` remains mandatory.
The activation shell clears its environment and installs the same explicit OS
allowlist as the worker. Its PID is persisted in `windows-stage/worker-launch.json`
and its exit is atomically recorded in `worker-exit.json` in the private stage.
The shell explicitly restores its process-scoped `Bypass` execution policy and
system-only `$PSHOME/Modules` path after clearing the environment; it does not
inherit caller module paths or alter persistent execution policy.
Breakaway intentionally escapes Host/Controller process-tree cleanup so activation
can survive a Controller restart. Use it only for a leader-authorized manual
rollout of the uploaded, hash-verified repository updater, never for Agents or
ordinary exec. It can leave an independent process after the caller exits;
`worker-launch.json` retains the exact PID for operator inspection and, if needed,
explicit termination after verifying its identity.
If the activation poll times out, the independent process may still be running;
inspect that exact PID and the receipts, never automatically launch it again.

Failed commands add `subprocess` to `result.json`: exit code, timeout/signal and
fixed repository-defined stdout/stderr error codes (plus byte counts when
available). Arbitrary output, argv, configuration, paths and credentials are
omitted. Unknown errors therefore remain explicitly undiagnosed; the receipt is
not a raw subprocess log. The Windows shell records only allowlisted exception
codes. `--resume` only polls the original job and cannot rerun a finished partial
job. After an operator verifies no worker remains and inspects the transaction
receipt, a corrected attempt uses a new job/receipt; never edit an old receipt
to make it runnable or restore an older startup intent over current state.
