# Windows runtime readiness: next release

2026-10-13. Local isolated work only; do not change running machines or the
previous release candidate. Windows runtime acceptance requires native MSVC CI,
not a macOS build or cross-target check.

## Small release patches

1. Exec OS baseline: existing `8711cf4`/`2b6398f` removes all inherited Treer
   identity, proxy and Node loader hooks while preserving exact Windows OS
   names. `6285d92` adds `HOMEDRIVE` and `HOMEPATH`, with negative identity tests.
2. Enrollment/root migration: an omitted new Windows `connect --root` selects
   `%LOCALAPPDATA%\Treer\root` (USERPROFILE fallback), creates a private directory
   and rejects an existing reparse chain. Unix still defaults to cwd. Existing
   enrollments retain their saved root; an explicit conflicting root is rejected
   rather than silently ignored. Add offline `service relocate-root --root ...
   --accept-workload-migration`, requiring a stopped Host and an existing target.
   Preserve machine ID, tokens, local address and saved startup state. No file
   copying, credential export, or service activation. Atomic individual config
   writes roll back on a reported second-write failure; service startup refuses
   mismatched Host/Controller roots after a crash, until relocation is rerun.
3. Native runtime gate: extend the existing real detached-Host fixture with
   Controller `exec_machine` plus actual `create(kind=command|app)` through Host
   ConPTY. Each runs Node 22, Windows PowerShell crypto, tasklist and Controller
   `service status`. Start the fixture from System32 with isolated LOCALAPPDATA,
   APPDATA/TEMP/TMP and synthetic stale identity names; never inspect installed
   credentials. Verify default root differs from System32 and inherited machine,
   operator, enrollment and UI variables do not reach Node children. A
   Host-owned temporary App runs `restart-controller`, exits successfully, and
   the Host/other workloads survive. Existing hot-restart/cold-stop checks remain.
4. Release-eng owns Windows updater scripts and workflow changes. Existing main
   `7ada02b` runs Controller/CLI file-transfer tests natively; the workflow's
   `windows_real_host` ignored-test filter must execute the wrapper, which invokes
   its isolated child once. Every PowerShell cargo command checks LASTEXITCODE.
   Native results and artifact SHA belong to the next fixed release manifest.

## Updating and activating safely

The Rust updater continues to reject Windows in-place updates. Renaming a live
executable can work, but is not a verified multi-binary transaction, rollback or
process-activation guarantee. A signed/hashed Windows staging and activation
helper belongs to release-eng; review it before enabling live replacement.
It must use private same-volume staging, validate all artifacts before execution,
retain old binaries, handle locked files without deleting the only working copy,
and verify the new Controller epoch/health before declaring activation success.
Host updates require an explicit cold restart: all Host-owned Agent/App processes
terminate, then saved startup intent restores them. In-memory provider sessions
are not reconstructed by Core; launchers must resume their saved thread/session.

Until that helper passes native tests, use a host terminal: verify and stage a
fixed release, stop affected services, keep a private backup of all three old
executables, install the complete verified set, and restore the same workspace.
If installation/activation fails, stop the failed service, restore the complete
old set and restore again. Do not delete machine configuration or re-enroll.
Do not use an Agent being terminated to supervise its own cold replacement.

Machine exec is Controller-owned, has a 30-second limit and loses its RPC when
that Controller restarts. `Start-Process`, nohup-style redirection and
DETACHED_PROCESS do not guarantee escape from Windows Job ownership. Reliable
Controller hot activation runs from a host terminal or a Host-owned managed
command/App; observe its saved output after reconnecting. A lost exec reply alone
does not prove restart failure. Never automatically retry a restart on that basis.
The launcher subcommand remains separate and never activates Host/Controller or
Agents; its embedded cross-platform Node entry needs no git, sh or npm on target.

## Root and file transfer

`treer machine show M` reports the authenticated root. Put/get REMOTE paths use
forward slashes relative to that root, with no drive, UNC, verbatim prefix, ADS,
device alias, symlink/junction or traversal escape. See the
[file-transfer design](2026-10-13-machine-file-transfer-design.md).
An existing System32 root stays System32 until the owner explicitly migrates it.
Never recursively copy System32. Copy only user-owned project data, retain the
private machine configuration, and check relative cwd plus absolute launcher
and executable paths before restoring. Tree folders do not move disk files.

## Evidence and review

Run platform environment and root relocation regressions plus focused Controller
tests, fmt/clippy and documentation checks locally. Native gate:

```powershell
cargo build --locked -p treer-agent-host -p treer-agent-server
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo test --locked -p treer-agent-server --bin treer-agent-server windows_
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo test --locked -p treer-agent-server --bin treer-agent-server windows_real_host -- --ignored
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
```

The first filter skips ignored fixtures; the second runs the wrapper only. The
inner fixture has a distinct name and must not run against ambient user state.
Release-eng reviews installation/root changes; security-claude reviews the local
environment and filesystem boundary. No native Windows success is claimed until
that gate has actually run. No push, deployment or managed restart in this task.
