# Quality And Maintenance

### Windows updater parity file fixture

The private Windows-only `windows_update_bootstrap::checked_files` fixture
creates new current-user/SYSTEM-only attempt directories and candidate files
with their DACLs at creation. It pins local NTFS directory ancestors and source
identity, streams at most 128 MiB through a 64 KiB buffer, hashes the copied
bytes and syncs/read-back checks the same destination handle. Native tests cover
checksum failure with retained evidence, existing-object and oversize refusal,
unsafe names/foreign parent writers, independent source offsets, handle
inheritance and write/rename sharing denial. Cross-compilation does not execute
these Windows behaviors; hosted Windows results must be recorded separately.
No installed-state read, publication, service restart or updater command is
wired by this fixture. A subsequent native rename fixture uses the same verified
object with read/DELETE access to move between three fixed attempt-local slots,
never replacing an occupied destination. Two new Windows tests cover identity
across activation/reverse rollback and second-rename failure with preserved
candidate/backup and collision refusal. Both new tests failed at the first rename
on hosted Server 2022/2025 at `ba665a24`; fixed-stage diagnostics are awaiting a
rerun and this slice is not passing. The earlier W1a hosted run does not cover
rename behavior. Durable receipt/recovery states
and production installation remain subsequent work.

### Windows self-update registration model

This historical model is not a prerequisite for the Windows PowerShell-parity
scope selected in issue #111 comment 5134; that scope makes no boot-freshness
claim and forbids registry/reboot experiments. The private `cfg(test)` platform module `update_registration` exercises a
synthetic, fixed-size registration format before native record publication is
implemented. Focused tests reject malformed headers, truncation/trailing bytes,
pipe-frame confusion, altered attempt/intent/process/boot identities and invalid
deadline windows. Independent readers compare the decoded fields with separate
synthetic observations; the owner retains its original expiry ceiling. The
version-2 fixture carries a nonzero 16-byte boot identity. A reader must compare
it with both its independent observation and every local clock sample; a changed
boot permanently refuses that reader even when ticks still fall in the window.
This tests rejection of prior-boot records with otherwise matching fields, not
the authenticity or availability of a Windows boot identity. Native registration
still requires verified boot identity acquisition and trusted clock samples.
An injected synthetic source tests boot/tick/boot read ordering and refuses
empty, mixed or failed samples without retry or fallback. Either deadline reader
permanently refuses after any acquisition failure; subsequent coherent samples
cannot renew its budget. These checks model acquisition ordering, not an OS API.
These tests do not read or write files, acquire OS identity or time, enforce replay
uniqueness, construct frame bindings, or grant update authority. Native record
publication, suspended-child registration and real-machine acceptance remain
separate gates. The byte layout is a fixture model, not a supported disk format.

### Organization invitation consent checks

The PostgreSQL-backed Proxy invitation tests use synthetic invitations in
isolated schemas. They cover explicit registration consent, member-only legacy
roles, concurrent single-use acceptance, unchanged existing members, cookie/Origin
and Bearer rejection, JSON OAuth start with the Console custom header, state and
browser-cookie binding, hash-only handoff records, expiry/replay, and session-bound
pending preview/accept/decline. CORS tests retain the exact Console-origin allowlist
and permit its OAuth custom header without permitting native-client headers.
Invitation-only password/new-OAuth registration without join consent is rejected
without creating a user; open-registration password behavior remains unchanged.
A removed pending table simulates storage failure after OAuth login and verifies
the resulting session remains valid and produces the session redirect cookie.
These fixtures do not log into a real IdP, consume a user's invitation, or prove
mixed-version rollout safety; upgrade all Proxy replicas before enabling the flow.

### App-layer deployment checks

`python3 -m unittest discover -s scripts/tests -p test_app_deploy.py -v` runs
the POSIX `scripts/app-deploy` fixture with disposable git repositories, real
Python App subprocesses, dynamic loopback HTTP ports, SQLite/WAL and a fake Treer
CLI boundary. It covers version/identity readiness on local and service-route
surfaces, stable identity, successful and failed migrations, SQLite snapshot
integrity/tampering, interrupted pointer-switch recovery, failed rollback blocking,
idempotent replay, locks, unsafe paths, dirty checkout exclusion and asset hashes.
An actual SQLite writer leaves committed candidate WAL behind; interruption
immediately after main-file replacement must expose only the backed-up state,
even before recovery is retried. This regression fails against the original
restore ordering. A separate test forces the SQLite backup progress callback to
expire and verifies exception propagation and unpublished-backup cleanup.
It never controls an enrolled App or existing Host/Controller. This is not a
native Windows test, authenticated multi-machine ingress test, malicious
same-account filesystem isolation test, or a guarantee of external-effect rollback.

For ordinary development, run only the focused checks that cover the changed
boundary. PostgreSQL-backed tests are optional unless the change directly
touches Proxy persistence, authentication, membership, Policy, or Core Message.
Do not start a database solely to satisfy a routine handoff; state which checks
were run and whether database-backed coverage was omitted.

Workspace audit PostgreSQL tests cover strict workspace scoping (excluding
organization-only events), pagination/filter validation, no-store including
errors, payload redaction and preservation of the old organization projection.
The permission matrix covers owner/org admin, direct workspace owner, member,
Agent/machine/service subjects, explicit enforce grants, monitor/default-allow
refusal and enforced owner denial; App tokens are audience/caller bound. These
tests do not imply complete event coverage, immutable audit storage, auditing of
audit reads or reconstruction of previously unattributed history. Mutation audit
regressions additionally exercise Tree/enrollment transaction rollback, repeated
and rejected writes, networking best-effort failures, future-scoped caller
attribution, immutable attribution for newly written deleted-workspace events,
and fail-closed payload projection. Networking and prompt audit delivery still
has no completeness guarantee; an acceptance event is not proof of turn completion.

Windows native artifact builds exercise ConPTY without an attached browser.
The Host answers the initial cursor inheritance query required by portable-pty
so child output can begin. The runtime tests cover split query bytes and the
native Windows test covers argv, environment, input, resize, and process stop.

Ingress browser binding has PostgreSQL regressions for browser-verifier
redemption, independent parallel flows, missing/wrong verifiers, atomic replay
fencing, expiry, hostname binding, and mixed-version code-table rejection.
`node crates/treer-proxy/tests/ingress-cookie-browser.mjs` additionally uses the
Web Playwright installation and Chromium to verify that a synthetic sibling
HTTPS host cannot plant or overwrite an `__Host-` verifier cookie. All browser
requests are locally fulfilled; no real Proxy, IdP, session or DNS is contacted.
This browser check covers cookie-prefix enforcement, not a deployed login flow
or other browser engines. Install the Web development dependencies first (a
worktree may use `NODE_PATH` pointing to an existing repository Web installation).

### Service environment and recovery validation

The macOS [multi-workspace fixture](research/fixtures/multi-workspace-per-machine.mjs)
builds two fresh workspace enrollments on an isolated Proxy/Postgres/NATS stack,
using private HOME/config/state/bin/root/socket paths. It covers separate machine
IDs, Agent/App creation, exec, put/get, launcher profiles, targeted lifecycle,
explicit bound-identity workspace/URL rejection, port-conflict preflight plus
same-key retry, and updater workspace/confirmation guards. It never operates on
real enrolled services. Build the four sibling debug binaries first, then run
`node docs/research/fixtures/multi-workspace-per-machine.mjs` from a private
worktree with Node 22+/npm, Docker and system Python available.

The known same-account boundary is tested using synthetic identities and only
booleans/status codes: A's exec can test readability of B's configuration
(`true`), connect B's Host socket (`true`, without sending a command), and reach
B's authenticated loopback API without credentials (`401`). No credential
contents are printed. These expected results document absence of same-OS-user
isolation; they are not claims that workspace credentials or 0600 provide a
sandbox. Windows/Linux multi-enrollment OS-service-manager execution remains a
separate native validation task. Python fake-machine tests cover confirmed
multi-install update ordering/rollback and rejection before activation without
confirmation; native Windows update is explicitly unsupported by that script.

Machine file-transfer coverage includes a bounded multi-chunk CLI round trip,
checksum rejection without publication, destination preservation/explicit force,
Controller caller/path/workspace binding, expiry/disconnect cleanup and a real
subprocess exiting without destructors followed by journal recovery. Controller
regressions also cover rejected reserved uploads followed by restart, legacy
reserved-namespace intents mixed with valid staging, stage/journal write failures,
and refusal to discard other malformed or unsafe recovery records. Reserved
intents are classified without filesystem access; recovery must never delete
private-state files even if their names resemble transfer staging. Platform
tests exercise hostile portable paths, symlinks/hard links and concurrent
atomic no-replace publication. Proxy PostgreSQL tests cover separate read/write
Policy denial and derived caller/body-free audit dispatch. These do not prove
snapshot semantics, isolation from a same-OS-account attacker, or complete
auditing across a filesystem/SQL failure. Native Windows execution is a separate
release gate; a cross-compiled platform check is not a native test result.
The Windows real-Host fixture isolates user config in a subprocess started from
System32, then exercises exec plus actual Controller-created Agent/App children
with Node, PowerShell crypto, tasklist and service status. A Host-owned restarter
must finish across Controller replacement; Controller-owned exec cannot guarantee
a reply across its own restart. Root relocation tests cover identity preservation,
live-Host refusal and recovery from mismatched configuration files. These tests
are acceptance requirements, not a claim that native Windows has already passed.
File I/O and recovery journal changes are serialized per Controller; a slow
filesystem can delay other transfers. Expiry skips a busy registry instead of
blocking the Tokio timer. Space checks are advisory, not capacity reservations;
aggregate staging is bounded by active-handle count times the per-file limit.
The Proxy limits in-flight transfer calls globally, not fairly per caller.

Updater executable staging closes and syncs the writable file before executing
validation. Unix validation retries only transient `ETXTBSY` within the existing
ten-second validation budget; permission and format errors fail immediately.
Linux regression coverage holds a real writable descriptor, confirms `ETXTBSY`,
then closes it and verifies validation succeeds. Isolated subprocess tests cover
both umask 022 and 077 without changing the Rust test runner's process-wide umask;
installation preserves restrictive user permissions rather than forcing 0755.

Unix cold-activation tests create stale Host sockets in an isolated child and
wait for its exit before probing. Dropping a listener in a parallel test runner
is insufficient: a concurrent fork can retain its close-on-exec descriptor until
exec, temporarily making the socket appear live. A duplicate-descriptor test
reproduces that distinction; cold activation still asserts no hot-restart call.
The explicit-port conflict/retry test is similarly isolated so inherited TCP
listeners cannot keep its released port busy during the retry assertion.

The shared `treer-platform` environment filter is used by nohup, foreground and
Windows detached launchers, daemon entry points (including launchd/systemd),
Host-spawned Controllers on crash/hot update, and PTY workload inheritance.
Generated launchd/systemd registrations contain configuration paths, not caller
environment snapshots. Explicit workload environment is applied after filtering.
An independent PTY subprocess regression starts from a polluted Host runtime
and verifies that old UI/identity variables are removed while a fresh explicit
workload identity and proxy entry survive.

`real_service_environment_boundary` runs isolated real Host/Controller binaries
with polluted identity/UI variables and a dead loopback proxy. It checks daemon
environment names, restores both Agent and App startup intent through a mock
Proxy, survives transient validation failures without reconnecting, and hot
restarts the Controller without replacing the workloads. It uses private
fixture state and never resolves installed services. Build sibling binaries
first, then run:

```sh
cargo build -p treer-agent-host -p treer-agent-server
cargo test -p treer-agent-server real_service_environment_boundary -- --ignored
```

Run this subprocess fixture on macOS and Linux; Windows cross-target checking
only establishes compilation, not native process-environment behavior.

### Virtual-host routing revisions

Virtual-host database polling preserves the routing revision when projected
content is unchanged. The PostgreSQL regression covers repeated empty and
populated refreshes, a remote service-port change, and deletion leaving an empty
workspace cache entry. Revision changes are routing changes, not polling ticks;
the revision remains local to a Proxy process rather than a durable global clock.

### Output retention and recovery

Restore reports record `Started` or `Attached` from the action taken under the
Controller recovery lock, not by comparing wall-clock timestamps. Host process
timestamps have millisecond wire precision; comparing them with a higher
precision request timestamp previously mislabeled fast spawns as `Attached`.
The mock-Host regression uses a fixed earlier timestamp across generations and
checks concurrent restore (one spawn, one attachment), lost-ACK attachment,
missed-exit respawn, and preservation of stopped/revoked startup intent.
Controller command-resolution regressions cover absolute-only PATH lookup for
Agent/App starts and recovery. Unix and Windows run an isolated child with a
cwd executable decoy, empty/relative PATH entries and no PATH; all fail closed.
Other cases cover fixed Windows `.exe`/`.com` selection, rejected UNC/device
search directories, regular/executable files, relative-command rejection and
unchanged explicit absolute commands. The mock Host startup/restart/App-update
fixture asserts absolute commands cross the Host boundary while logical command
names remain in durable startup intent. Native Windows execution remains a
release gate; a cross-target compilation is not native execution evidence.

Controller inventory reconciliation must preserve the cached output and its
revision for the same process generation and stream epoch, including exited
Agent and App instances. An inventory-only update is not an empty transcript.
Previously it rebuilt exited entries with empty text on each recovery scan,
while retaining their nonzero output revision; short-lived Canary probes could
therefore finish successfully but appear to produce no output.

The Controller regression fixture covers output followed by exit, repeated Host
inventory scans, and a replacement Controller replaying exited Agent and App
output from a surviving Host. Same-run revisions cannot move backwards; a new
run must not inherit the previous run's text. Output updates and inventory
replacement share the existing entry lock to avoid losing concurrent output.

Host replay is bounded and in memory, not a durable terminal log archive. A
Controller hot restart can recover retained output from the surviving Host,
subject to replay/history limits. After a Host cold restart, output of exited
processes from the old Host is unavailable; restoring startup intent does not
restore that transcript. This limitation is separate from inventory incorrectly
erasing output that the current Host/Controller still retains.

Run the complete gate on demand or while preparing a release:

```sh
just test-db-up
export TREER_TEST_DATABASE_URL=postgres://treer:treer@127.0.0.1:55432/treer_test
just check
```

The complete gate is not a prerequisite for every local change. When its
database coverage is relevant and Docker is unavailable, record that the
PostgreSQL-backed workspace gate was skipped. Do not point
`TREER_TEST_DATABASE_URL` at a shared or production database just to make the
local gate pass.

Pull requests and pushes to `main` run [CI](../.github/workflows/ci.yml).
The Linux Rust job builds the workspace, checks formatting, runs
`cargo test --workspace --locked` against a healthy PostgreSQL 17 service, and
runs Clippy for all targets with warnings denied. Each database test uses its
own schema; connection failures fail the job rather than skipping coverage.
An independent Node 22 / pnpm job installs the frozen frontend lockfile and runs
`pnpm typecheck` and `pnpm build`. CI requires no deployment credentials and
does not publish artifacts or deploy. Native macOS/Windows PR gates and browser
end-to-end CI remain separate gaps.

Use Slurm for CPU- or memory-heavy builds, Clippy runs, frontend builds, and
non-database workspace tests when the checkout and toolchain are available on
the compute nodes. Slurm does not remove the PostgreSQL requirement: the job
must still receive an isolated `TREER_TEST_DATABASE_URL`, either from a
cluster-provided database or from a supported container runtime. If neither is
available on the allocated node, skip the PostgreSQL-backed tests and report
that limitation instead of treating the partial run as the complete gate.

When explicitly requested, `just check` verifies documentation links, release
tooling, the control-plane frontend, updater tests, the shared Agent launcher,
Rust build/format/tests, and Clippy with warnings denied. Other Workspace Apps
are intentionally outside the release gate; run their focused
checks when changing them. Focused commands are:

```sh
just app-test
just agent-launcher-test
just updater-test
just messaging-e2e
just web-test
(cd web && pnpm run check:contrast)
just ais-e2e
just service-canary
just machine-artifacts-test
node --test scripts/install-path.test.mjs
cargo test -p treer-proxy message_
cargo test -p treer-proxy -- updater::
cargo test -p treer-proxy -- admin_update_
node scripts/check-docs.mjs
```

## Evidence Matrix

| Area | Present evidence | Remaining gap |
| --- | --- | --- |
| Rust | Linux PR workspace build/tests with PostgreSQL, format, strict Clippy | No native macOS/Windows PR gate; first GitHub run pending |
| Frontend | Control-plane and Mail typecheck/build; mocked desktop/mobile browser workflows | No screenshot visual regression suite |
| Core Message | Store/API/CLI tests for DAG visibility, delivery, idempotency, Policy revision, body-free outbox, and migration | No retention/export/delete, attachments, or load/failure suite |
| App identity | OAuth/PKCE, audience, membership/service invalidation, directory, App Message routes, 60-second browser forwarding and sanitization; Issues iframe/ingress workflows at desktop/390px through a local gateway double | No refresh-token contract or deployed automatic-identity browser acceptance |
| Unified workload lifecycle | Stable identity/generation, single Controller recovery owner, real Host cold/hot recovery and concurrent start, stopped-intent persistence, legacy App migration, and mixed browser tree/lifecycle | PTY-backed runtime; no health check, deployment revision, or durable terminal log archive |
| Mail | Python HTTP contract, frontend build, resumable SQLite/PostgreSQL migration | No unattended real-browser audit |
| Telegram | Fake Bot API, mapping, ack crash ordering, restart, rate-limit and ambiguous-send tests | No live Telegram canary, webhook, or active-active mode |
| Distribution | NATS event/outbox and multi-Proxy routing tests, including terminal epoch/cursor delivery and bidirectional network payload/accounting | No automated partition/failure CI |
| Host supervision | nohup default and PID identity, explicit systemd selection, partial-unit repair, stale-registration cleanup, TUI/Web diagnostics, and cold-Host restoration and explicit service probes | No normal macOS lifecycle CI; no Apple container machine setup CI |
| Machine connection UX | Service workspace listing, `/api/health` `proxy_connected`, duplicate/stale reconnect, real NATS lease-expiry recovery and replacement fencing, Proxy ping idle close, connect reuse, bind conflict, explicit plain-mode migration and service forwarding | NATS tests require isolated `TREER_TEST_NATS_URL`; no physical lid-close in CI; record a manual macOS sleep/wake check after deploy |
| Release | Five-platform metadata/checksums, native Windows build/test job, GHCR publish workflow, signed-manifest Node tests, and Controller signed-channel/manifest plus binary SHA256 verification | Windows runner results pending; no updater downgrade/freshness enforcement; trusted release key must be provisioned locally |
| Self-host update | Updater unit tests; Proxy admin forward tests; `/admin` e2e | No unattended Compose apply against live Docker |
| Security | Explicit trust tier and Policy tests | Allow-by-default Policy; external isolation is operator-owned |

## Review Triggers

Native Windows changes require the Windows MSVC job in
`.github/workflows/build-release-artifacts.yml`: local IPC reconnect and owner
checks, private state replacement, PID identity, ConPTY argv/input/resize/stop,
real Controller/CLI auth, detached Host idempotency and process survival across
Controller replacement, plus native installer checksums/PATH. A cross-target
`cargo check` or Clippy run on macOS only establishes compilation. Real Windows
enrollment, HTTP service routing, App deployment, reboot recovery, and descendant
cleanup under failure still require native acceptance before announcing support.
Windows in-place binary update, automatic login tasks, and system services are
outside the initial implementation.

| Change | Minimum focused evidence |
| --- | --- |
| Shared protocol | Round-trip tests, legacy registration negotiation, unknown-command connection survival, plus affected endpoints |
| Proxy auth or membership | Authorization, revocation, and cross-workspace tests |
| Core Message | DAG/visibility, delivery, idempotency, ack, Policy, outbox body exclusion, and migration tests |
| Mail or Telegram | App unit tests, external API fixture, restart/migration, and frontend build when applicable |
| Host/Controller lifecycle | `just service-canary` plus idempotency and process-survival tests |
| Unified App/Agent lifecycle | Stable service/identity transaction, generation fencing, concurrent start/recovery, explicit stop persistence, legacy adoption, CLI parsing, mixed tree persistence and desktop/mobile workflow |
| Network or ingress | Authentication/header, streaming, WebSocket, endpoint ownership, Agent cross-owner denial, and Canary coverage |
| Workload HTTP entries | Per-entry signed identity, inherited environment, credential/header stripping, denied opens, HTTP/WebSocket/CONNECT, name resolution/rename with stable Policy IDs, stop cleanup, and hot/cold restoration |
| Browser workflow | Typecheck/build, CORS/return-path checks, desktop and mobile validation |
| Agent Interface adapter | Adapter unit tests plus `just ais-e2e` when a live Treer and vendor binary are available |
| Voice ASR or command | Proxy `voice` / `voice_llm` tests plus authenticated `/voice/command` route tests; optional `TREER_VOICE_LLM_LIVE_TEST=1` against a configured upstream |
| Native iOS/Android fleet | `just mobile-ios-ci`, `just mobile-android-ci`, Android `CreateFlowTest`, iOS `CreateFlowTests` / `TreerUITests`; live AOSP + iOS simulator login/create-machine/create-agent/prompt when a Proxy and Host are available |
| Documentation | `node scripts/check-docs.mjs` |
| Release publishing | `just machine-artifacts-test`, `node --test scripts/release-r2.test.mjs`; hosted downloads must match all twelve binaries without redirects; isolated R2 verification when publishing signed manifests |
| Installed Controller updates | Node/Rust signature compatibility, tampered/missing metadata, trusted-key binding, channel/manifest/path binding, duplicate/missing artifacts, size limits, binary SHA256 failures before execution, and existing staging/recovery tests |
| Self-host GHCR or updater | `just updater-test`, Proxy admin update tests, and `/admin` e2e |

Documentation describes the current source, not future intent. Dated research
is historical and does not override implementation, protocol types, or tests.

Retired transparent-network labs under `scripts/network-lab/` and dated research
are historical fixtures, not supported acceptance gates. The native macOS helper
and its build gate have been removed.

Current focused transport and recovery checks:

```sh
cargo test -p treer-agent-server network
cargo test -p treer-agent-server workload_proxy
cargo test -p treer-agent-server --test agent_address
# Optional real HTTP client check; requires curl on PATH:
cargo test -p treer-agent-server curl_uses_inherited_proxy -- --ignored
cargo test -p treer-agent-server recovery
cargo test -p treer-cli --test session
cargo test -p treer-proxy network
```

The subprocess fixtures require freshly built sibling executables:

Recovery fixtures assert privileged inherited credentials are **absent**, rather
than replaced with empty environment entries. A panic in a fake Host can close
its RPC and make the Controller report `Failed`; distinguish that fixture
failure from a production restoration failure. The isolated real-Host cold
restart test additionally checks Agent and App child-process identity bindings,
absence of stale machine/operator/enrollment/UI environment, stable workload
credentials, and that stopped/revoked workloads are not relaunched. Child output
contains identity markers and SHA-256 of fixture-generated credentials only,
never credential values; this ignored Unix fixture also requires `openssl`.

```sh
cargo build -p treer-agent-host -p treer-agent-server -p treer-cli
cargo test -p treer-agent-server recovery_real_host_cold_restart -- --ignored
cargo test -p treer-cli --test controller -- --ignored
```

The local Linux Docker regression uses real authentication, PostgreSQL, NATS,
two Proxies, and non-root machines. It checks installation dependency preflight,
installed CLI discovery, Agent messages, explicit service forwarding/publication,
App access, per-workload HTTP identities, verified local HTTPS/CONNECT,
WebSocket upgrades, listener cleanup, and hot/cold restoration:

```sh
docker build -t treer-local-e2e:fixed .
node scripts/docker-core-e2e.mjs
```

Requires Node 20+ and Docker; `TREER_E2E_IMAGE` can select another freshly built
image. The runner downloads Debian dependencies, binds host listeners only to
loopback, and stops its own containers in a finalizer. It retains stopped test
containers, the network, and logs/results in the printed temporary directory for
inspection; `resources.json` lists their exact names. It does not deploy Canary
or touch existing containers. Fixtures use Python services, without vendor Agent
CLIs. Browser, public DNS/TLS, and OS boot-manager checks remain separate.

Proxy persistence tests require an isolated `TREER_TEST_DATABASE_URL`. Set
`TREER_TEST_NATS_URL` to an isolated broker for cross-Proxy tests; without it,
a test that returns early is not distributed-path evidence. Configured raw-TCP
allocation tests additionally require an isolated port range and are ignored by
default. Local cold-Host tests do not prove physical reboot, OS boot-manager,
public DNS/TLS, or production firewall behavior; record those separately.

Machine exec environment regressions use a polluted synthetic parent and the
real `ControllerRuntime::exec_machine` subprocess path. They assert absence of
all Treer/UI variables, including former empty credential placeholders, and
verify Agent/App launch templates retain only their own workload identity.
Windows native tests additionally run Node arithmetic and baseline assertions,
Windows PowerShell crypto initialization, and `tasklist` using the same exec
path; Node 22+ must be installed in that CI job. Filtered Host/PTY inheritance is
tested independently with synthetic machine/operator/enrollment credentials.
Unix runs do not constitute native Windows execution evidence; record the
Windows CI result separately before claiming that platform verified.

The ignored `windows_powershell_exec_diagnostics` probe records only synthetic
output, names/existence, exit codes, timeouts and durations. Native runs
38477160912/38477787941 showed that no PSModulePath caused output/RNG probes to
time out, while adding that one variable restored them with either NUL or a
closed input pipe. Ordinary versus verbatim cwd made no difference; Node worked
with both. No production cwd conversion was added. Regular Windows smoke tests
retain strict exit/timeout assertions and exercise version, presence and RNG;
an actual Controller-spawned Node/Rust child asserts the initial module path
equals the fixed system directory, while PowerShell asserts that its startup
search still contains it. Native PowerShell may add account-default directories;
that does not excuse a failure of the initial environment equality assertion.
Diagnosis is not the full release gate.

Hosted Windows runner PowerShell 5.1 auto-loading without the image's
pre-generated module analysis cache took 12–28s per invocation on both
`windows-2022` and `windows-2025`, sometimes exceeding the unchanged 30s limit.
A new private cache appeared but did not show measured growth or fast reuse;
the default parent directory was present, creatable and actually writable.
The precise cache-reuse interaction remains unexplained. This does not justify
product cache management or a broad environment allowlist.

Windows 11 manual fixed-module probes reported by leader took 130–144ms;
new explicit-cache probes took 421/263/265ms. Exact `e3ed8ea` compiled identity
and PS/Node tests on win_native passed in 0.02s and 0.79s respectively. These
are product-environment observations, not universal performance guarantees.
Test hosts must supply the standard OS baseline (ProgramFiles, COMPUTERNAME,
the installed PowerShell executable's PATH, and PSPATH if required by their
tooling). Missing host baseline is not repaired by the cache compensation.

Only the isolated in-process Windows exec unit-test fixture may supply the
test runner's `PSModuleAnalysisCachePath`, with both `CI=true` and
`GITHUB_ACTIONS=true` and a supplied value. An absent value stays absent.
The Controller field and use are under `cfg(all(test, windows))`, default to
no opt-in, and the setter is in the test module. Production binaries contain
no field, setter, CLI/API/configuration option or environment read for this
compensation. Real-Host/integration tests do not opt in. The production
inheritance allowlist still rejects the variable; initial PSModulePath remains
the fixed Win32 system directory. No OS-token block reconstruction is included.

Threat scope: a caller-controlled cache can affect module discovery and cache
writes; inheriting it is inappropriate product behavior. Only an isolated
hosted runner's image configuration is trusted here; CI flags are accidental-use
guards, not authentication. Do not inject secrets or use untrusted runner cache
configuration. This compensation is not clean-machine performance evidence.

Record environment correctness separately from performance: strict initial
PSModulePath equality, credential/proxy/loader-hook absence, Utility/version/RNG
exit 0 and no timeout under the original 30s ceiling remain required. Per-probe
durations record hosted-image behavior; CI green/red is not a performance
guarantee. Keep Windows 11 evidence and the real Host/CLI/install/update/UI
gates alongside this compensated unit test. Do not remove Utility/RNG coverage.

Windows PowerShell coverage has two explicit layers. The in-process Controller
unit fixture retains `Write-Output`/`New-Object` Utility/version/RNG cmdlet
regressions, fixed initial PSModulePath equality, identity absence and the 30s
limit; only that isolated fixture can opt into the hosted-image cache. Separate
Windows 11 exact-test evidence runs without CI flags or cache compensation.
The real-Host wrapper uses freshly compiled **production** Host/Controller
binaries, with no test hook or cache injection. Its PS smoke uses CLR/Console
APIs without automatic Utility-module loading, checks PS version, OS baseline,
presence of the fixed system Modules directory and CLR RNG, then requires exit
0/completion markers through machine exec and Host-owned Agent/App paths. Its
30s exec ceiling remains strict. Since native PowerShell can append account
directories, post-startup path presence is distinct from the Controller unit
test's initial environment equality check.

This is deliberate coverage separation, not a claim that hosted real-Host tests
exercise cmdlet auto-loading or predict its latency. That regression remains
covered at Controller level and on Windows 11; a deployment needing real
production Host + Utility-module coverage needs the uncompensated native-machine
acceptance result too. The wrapper retains process ownership, detached restart,
stop, canonical root and parent-traversal rejection assertions. No production
environment hook or change of platform is used to satisfy this gate.
