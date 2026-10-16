# Controller self-update transaction

Design for Issues #111, 2026-10-16. P0 only: implementation waits for security
review. Baseline: `2bb65dbe323457e8be6779c36d7469885e2433d5`. This is proposed
behavior, not a claim that Windows self-update already works.

## Scope and reuse

One Rust transaction engine in `treer-agent-server self-update` replaces the
Python/PowerShell machine updater. The coordinator submits a job and polls its
receipt; launcher installation continues to use the existing embedded launcher
installer, not a second implementation. Never replace or restart the Host;
record its PID, creation identity and inventory before/after activation. Running
Agents retain process identity, generation, output and launcher assets.

Existing code to extract and reuse:

- `crates/treer-agent-server/src/release_update.rs`: locally provisioned SPKI
  Ed25519 key, strict detached signatures, channel/manifest/artifact bindings,
  duplicate rejection, bounded downloads and binary digest verification.
- `service.rs::update`, `stage_executable`, `activate_controllers` and
  `restart_controller_at`: verify both binaries before executing either, close
  write handles before validation, Unix replacement, epoch-based hot activation
  and rollback. Current update explicitly rejects Windows and has no durable
  transaction journal or anti-rollback state; these are new work.
- `treer-platform`: private files/directories, atomic `replace_file`, process
  identity and locks; extend platform mechanics here. Current helpers alone do
  not prove a whole path is free of symlinks/reparse points: validate ancestors
  and use checked handles for mutation.
- `launcher.rs`: verified embedded payload, version/capability contract, trusted
  absolute Node 22+ selection, profile preservation and rollback.

CLI proposal: `self-update --workspace W --sha FULL_SHA --sums-sha256 DIGEST
--artifact-base HTTPS_URL --node ABS_NODE [--accept-shared-binaries]`, or mutually
exclusive signed `--channel stable --release-base-url HTTPS_URL --public-key KEY`.
`--status JOB_ID` reads a local receipt; there is no arbitrary receipt-file or
internal worker executable override. Require exact enrollment selection when
ambiguous. Derive destinations from the selected installed configuration and
validate them; do not select binaries from ambient PATH or workload `TREER_BIN`.
No wire change is required for initial local CLI execution/status polling.

Pinned mode verifies exact SHA256SUMS bytes against the supplied digest, then
strict expected filenames, size bounds and every installed binary digest.
Distinguish release SHA from artifact source SHA when releases reuse native
artifacts: compare binary identity with the authenticated artifact identity,
not blindly with the release wrapper SHA. A bare SHA alone is not provenance;
the caller supplies a trusted checksum pin/receipt. Signed mode keeps its local
key trust root. Neither mode fetches a trust key from the distributor. Both use
no ambient proxy, no redirects, no URL credentials/query secrets, bounded
timeouts/body sizes, and production HTTPS. Loopback HTTP is fixture-only.

## Transaction and Windows handoff

1. Acquire an installation-wide cross-process lock (shared binaries span
   workspaces), then selected-enrollment activation lock in that order. Enumerate
   enrollment names without exposing credentials; require explicit shared-bin
   acknowledgment if another installation shares the destinations. Activate only
   the selected Controller; warn that other workspaces use new binaries on their
   next start. Recheck paths, free space, running Host identity and capability.
2. Create a UUID job below the configured Treer state root, outside the workload
   root/shared TEMP. Unix directories/files are 0700/0600; Windows DACL is current
   SID plus SYSTEM. Reject symlink/reparse chains and unexpected existing files.
   Download, verify, stage and smoke-validate before any destination replacement.
   Executables get required executable mode after verification, before launch.
   Close and sync files; same-volume sibling staging is needed for atomic rename.
3. Back up Controller, CLI and startup store privately; hash each and record the
   startup store revision/hash. A backup contains credentials and never travels
   through Proxy, stdout or the public receipt. Do not modify startup specs as
   part of Core activation. Save durable `prepared` receipt before replacing files.
4. Start a verified, job-local activator (a copy of the installed Rust Controller,
   with matching hash) with sanitized service environment, null stdin and private
   bounded logs. The activator owns the transaction lock for the full operation;
   parent and child use a ready/ownership handshake, not an unlocked interval.
   Parent returns accepted/job ID only after durable handoff. A lost exec response
   is ambiguous: poll the existing job, never automatically submit another one.
5. On Windows use `CREATE_BREAKAWAY_FROM_JOB | DETACHED_PROCESS |
   CREATE_NEW_PROCESS_GROUP`, no inherited handles, and verify the activator is
   outside any Job before acknowledging handoff. These flags are a request, not
   proof: existing Host Jobs may prohibit breakaway. If prohibited, fail before
   mutation with `activation_requires_host_terminal`; use a trusted host terminal
   or a separately reviewed Host-owned activation mechanism. Do not fall back to
   CIM/PowerShell, silently alter Host Job policy, or claim arbitrary `treer exec`
   can always survive its Controller's termination. This limitation is an explicit
   P2 acceptance question for the existing deployed Host, not a reason to restart it.
6. Activator writes `activating`, retains verified old Controller/CLI under unique
   `.prev-<sha>-<job>` names, then places verified replacements. Windows uses checked
   native rename/replace operations: running image sharing/antivirus denial can
   fail; bounded OS-sharing retries cannot bypass failed verification. Journal
   intent and completion for each individual rename so a crash between old-image
   rename and new-image placement is recoverable. Unix renames staged siblings
   over destinations and syncs parents. No two-file atomicity is claimed.
7. The **existing Host** receives its ordinary restart-controller request and
   starts the configured installed Controller, not a second manually spawned
   Controller competing for the machine lease. The detached activator remains
   alive as watchdog. Check new Controller epoch, expected binary source identity,
   Proxy connection, and unchanged Host identity plus workload IDs/PIDs/generations.
   Missing old-Controller health/contract capabilities fail preflight explicitly.
8. Invoke verified new Controller `launcher install --workspace W` with trusted
   Node/CLI paths. Verify manifest capabilities, profile readback and installed
   version. Do not restart Agents or rewrite customized profiles without consent.
   A separate smoke Agent requires explicit test authorization, is deleted only
   by its returned stable ID, and is not confused with preserving existing Agents.
   Core healthy but launcher failed yields `partial`, never `complete`; preserve
   receipts/backups and require an explicit follow-up or rollback decision.

The internal activator entry is not an authorization boundary: a same-account
process can already replace these binaries. Nevertheless it accepts only a
validated private job ID, revalidates hashes/paths/state transitions, and cannot
become a generic command execution or process-breakaway API. Policy `machine.exec`
remains the remote submission gate. One OS account is one trust domain.

## Receipts, rollback and crash recovery

Receipt schema v1: `job_id`, monotonic `revision`, `workspace_id`, `server_id`,
`release_sha`, `artifact_source_sha`, source kind, verified binary hashes,
`phase`, start/update timestamps, bounded fixed `error_code`, per-step outcomes,
before/after Controller epochs and Host/process invariants. Phases:
`preparing -> prepared -> activating -> core_healthy -> launcher_installing ->
complete`; failures terminate as `failed`, `rolled_back`, `rollback_failed` or
`partial`. No URL secrets, argv, config content, environment values, raw child
output or startup credentials. Private transaction metadata additionally stores
checked destinations, backup hashes and rename progress; only fixed safe fields
are exposed to the coordinator. Receipt reads never authorize arbitrary file read.

Write temporary receipt in the same private directory, close/sync, native atomic
replace, sync parent where supported. On NTFS validate effective ACL/owner and
protection after replacement (equivalent duplicate ACEs are not permission
expansion). Retain previous receipt until replacement succeeds; never delete it
first. Faulted writes must not produce success or a missing receipt. Power-loss
durability limitations of each OS/filesystem must be documented from tests.

On Core activation failure restore both verified old binaries and ask the same
Host to restart only its Controller; report `rolled_back` only after old epoch,
connection and workload invariants pass. If restore/activation fails, retain
backups and report `rollback_failed` with an explicit manual recovery command.
Do not overwrite a concurrently changed startup store: normally it was never
modified; any necessary restore requires revision/hash compare-and-swap under
the recovery lock. Refuse incompatible persisted-schema changes before activation
unless forward/backward compatibility is proven. Launcher failure does not
blindly restore a stale startup backup over legitimate concurrent edits.

After activator death, the next explicit `self-update --recover JOB_ID` reconciles
journal, actual hashes and live process identities under the same locks. Never
infer success from a PID or an old receipt alone; never restart the Host or replay
a partial launcher/smoke operation implicitly. Keep completed backups/receipts
under an explicit retention policy: keep the latest three completed transactions
by default, never evict an unresolved transaction automatically. An explicit
cleanup command removes only journal-registered, validated exact job directories;
retain any job whose safety cannot be established. Backup paths live in the Treer
reserved namespace denied by machine file transfer.

Signed-channel P4 adds a durable highest accepted release sequence per trusted
key/channel and signed monotonic metadata; current version strings do not supply
that property. Reject replay/downgrade before executing staged code. Transaction
rollback to its verified previous image is distinct from accepting an old network
release and must not lower the high-water mark. Legacy metadata lacking the new
capability fails clearly once protection is enabled; key rotation/recovery needs
an explicit local trusted procedure. P1/P2 must not claim anti-rollback protection
until P4 ships. Owner rescue/Provider authorization are unchanged.

## Security review questions and refinements (comment 1676)

Use existing HTTP/Ed25519/SHA-256 dependencies; no additional network client.
Staging must stream into exclusive no-follow files and hash incrementally.
Windows verified files remain protected from concurrent write/delete through
checked handles until publication; receipt/binary mutation uses those verified
identities, not a later unchecked reopen. Test Host executable bytes as well as
PID/creation identity unchanged. Bind launcher workspace, server and URL explicitly.

The future signed sequence document needs an expiry, authenticated sequence and
local highest accepted value; advance the value only after healthy activation,
never lower it on rollback. Explicit same-version/downgrade recovery is local
operator break-glass, recorded in receipts and audit. Pinned SHA has no intrinsic
ordering: require an explicit `--allow-same` or `--allow-downgrade` acknowledgment
when the operator cannot establish forward order from authenticated metadata.
Audit submission and outcome with actor/mode/hash/sequence/fixed code, not paths
or credentials. Offline audit delivery must not erase the durable local receipt.

Leader decision 1684 and security approval 1687 resolve the P0 questions:

1. The target is signed-only. Release will publish every `canary_passed` build
   to a signed `canary` channel with monotonic sequence metadata. Any new dedicated
   remote endpoint accepts signed manifests only. Until that channel exists,
   exec-origin pinned updates require `--allow-pinned`, are marked unsigned in
   receipt/audit, and default to the enrolled Proxy origin. Another origin needs
   explicit `--allow-artifact-origin` acknowledgment recorded in the receipt.
   Remove pinned mode in P3; do not release GA with this transition remaining.
   No fictitious operator-only environment guard is added: authorized machine.exec
   already permits arbitrary code as that OS account, so pins trust the authorized
   caller and its release manifest, not an independent publisher signature. This
   does not resist a compromised Proxy or a same-account Agent. Do not require
   a human credential in argv/environment to create an unenforceable boundary.
2. Build/hash checks of the staged successor happen **before** old Controller
   termination; Host-owned sequential respawn remains the lifecycle authority.
   A running successor cannot first connect with the same machine credential and
   then terminate the old Controller without split-brain risk. A preflight-only
   successor may prove executable identity over local IPC but must not open a
   Proxy connection. Existing Host restart stays wire-compatible; any stronger
   authenticated handoff requires explicit Host capability negotiation. On
   startup detect unfinished transactions and refuse ordinary activation until
   deterministic reconciliation; do not implicitly replay launcher/smoke work.

Use OS advisory locks (`flock`/`LockFileEx`), released on process death, with the
installation-before-enrollment ordering above. `--recover` takes those same locks
and refuses an active activator. Job selectors are UUIDs only, never paths. Keep
activator logs private and size bounded with fixed child failure codes; no raw
child output, environment values or URL queries. P1 health assertions compare
both source commit metadata and the installed image digest, and audit-delivery
failure tests must prove the local receipt remains intact. Before P2, obtain an
authorized non-mutating breakaway probe on the deployed Host and record it in
the ticket; approval of this design is not approval of Windows implementation.

Additional fault cases: truncate downloads, replace checked parent directories,
crash during rollback, expired signed metadata and any same-version/downgrade
override without operator acknowledgment. Neither an operator flag nor a local
receipt grants new remote permissions.

## Phases and acceptance

| Phase | Deliverable / gate |
| --- | --- |
| P0 | Security1687 conditionally approves Unix P1; leader1684 decisions recorded here. Launcher coordination review remains requested. |
| P1 | Shared verifier/transaction/receipt engine + Unix activator; private fake HTTP/signatures, fault injection and real isolated Linux/macOS handoffs. |
| P2 | Windows native filesystem/ACL/lock/Job helpers; hosted NTFS tests plus authorized win_native exact official-binary apply, launcher install and smoke. Establish whether existing Host permits breakaway before promising exec-origin activation. |
| P3 | Signed Canary channel live; remove pinned mode. Coordinator submits/polls only; migrate per-machine receipts. Remove PS/Python engines only after replacement feature parity and platform acceptance; no double activation or fallback to legacy after ambiguous success. |
| P4 | Signed sequence/replay protection, key-rotation and backward-compatibility tests; clearly advertise capability. |

Dependency correction from security1690: the phase numbers do not authorize a
P3-before-P4 rollout. P3 signed-only cutover and removal of pinned mode require
P4 sequence/expiry verification plus a durable local high-water mark to be
implemented and tested first (or delivered in the same reviewed cut). The signed
Canary publisher and consumer must agree on these fields before cutover. P1
journal schema reserves nullable accepted key/channel/sequence/expiry fields;
null means protection is unavailable, never sequence zero or permission to
downgrade. Legacy signatures alone provide authenticity, not freshness or replay
protection. Until the dependency is satisfied, keep the documented transition
explicit and do not advertise the signed-only target as complete.

Required deterministic tests: bad/missing/duplicate SUMS, wrong digest/signature/
platform/source identity, redirects/URL credentials/ambient proxy; path traversal,
UNC/verbatim/ADS/reserved names/reparse replacement races; inherited NTFS ACLs,
true-null/ReplaceFile semantics, locked running images; every rename/receipt crash
point and ENOSPC; stale PID, lock contention and two workspaces; activation timeout,
old Controller rejection, failed rollback and startup-store concurrent edits;
detached watchdog survives exec/Controller exit or rejects before mutation;
unchanged Host/Agent inventory and output; Node missing/profile customization and
launcher failure produce truthful partial receipts. No timeout relaxation or
retry-until-green substitutes for these checks.

Use one Core Cargo cache, debug info disabled and hermetic private PG/NATS where
needed. Core author gate is full `just check`; release gates exact SHA and all
five platforms, then authorized real-machine receipts. Hosted shell/ACL fixtures
cannot stand in for real process activation. On win_native all writes must stay
inside leader-approved Treer paths, exact paths are posted before each apply,
changed paths and receipts afterward; no Host restart, system changes or reads of
personal files. P0 performs no machine operations.
