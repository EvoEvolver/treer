# Controller update integration boundary (#111 / #222)

This is a source-integration inventory, not a production activation plan or a
claim that the complete acceptance branch is suitable for merging into main.

## Fixed inputs

- Verified remote main: `c0ebb797a7d5d81e7c5c12052e7849e8742b2a5b`.
- Accepted runtime source: `ad23f3bfeb9c9b1d47e6c07d6f8dab05cd3a08bf`.
- Receipt documentation: `1a0e22e7fc0726f43c2921b11439ac7c6e669173`.
- Actual browser/native/PG run: `40197073704`, review #222:8917.
- Merge base of main and receipt branch:
  `2264f52adfdd827ea905b7428436a9b19a8e572a`.

The runtime and main have diverged across unrelated features. A whole-branch
merge, whole `crates` tree import, or whole `web` tree import would overwrite
unrelated accepted behavior. The accepted source must remain immutable as
evidence; any main-based composition is a different source with a separately
identified integration delta.

## One integration line

Core owns a new main-based source composition, with ordinary capability OFF.
First inventory the new updater modules and their shared registration hooks;
then compose those hooks against canonical code without importing unrelated
tree state. Web verifies only the Console hooks it owns. Release receives one
composed source after review; it does not repeat the completed isolated stack
merely to reconstruct its evidence.

Candidate module families are the Controller `self_update.rs`, `self_update/`,
`controller_update.rs`, `controller_update/`; protocol `controller_update.rs`
and `controller_update_runtime.rs`; Proxy `api/agents/controller_update.rs` and
`auth/controller_update.rs`; Console `controller-updater.tsx`,
`controller-update.ts`, and `controller-update-client.ts`. Their focused fixture
modules and maintained research accompany the corresponding runtime modules.
These are an inventory, not an unconditional copy allowlist: dependency closure
must first be checked against main.

The following existing files need updater-only hunk composition, never wholesale
replacement from the acceptance branch:

- Controller Cargo features, `main.rs`, `service.rs`, `proxy.rs`, `host_client.rs`,
  and launcher/provider installer reuse hooks.
- Platform Cargo dependencies and `lib.rs` registrations for private storage,
  retained images/processes, bounded children and lock transfer. Separate the
  Unix dependency closure from unwired Windows bootstrap work.
- Protocol module/command registrations; Proxy API/auth module registrations,
  routes, schema migrations, state/command plumbing and test-only counters.
- Console Machine entry, translations and Web test configuration/documentation.

## Preserve canonical components

Do not import Host or host-protocol state from this branch. The branch-to-main
diff includes Host changes from other historical work; those are not authority
to change the current Host under this integration. Likewise, do not import
launcher sources/public assets, App implementations, endpoint/message routing,
metrics, recovery screens or unrelated Console screens from this older tree.
Any necessary overlap must be identified by symbol and reconciled in canonical
source by its owner.

No ordinary capability flip, production artifact publication, release
installation, enrolled lifecycle or legacy updater deletion is part of this
composition. Windows runtime support, signed-channel replay protection and
coordinator cutover remain separate #111 leaves. The completed Linux browser
leaf is not rerun or relabeled as those capabilities.

## Current next action and stopping condition

The next concrete action is a main-based dependency/hunk composition, starting
with the Unix updater and its existing protocol/API registrations. Recheck the
remote main SHA before creating that candidate. Stop and name an exact missing
module or incompatible canonical hook if dependency closure cannot be preserved;
do not use an unrelated older tree to make a build pass. This inventory itself
has no external-resource or user-approval blocker. A future ordinary activation
requires a separately concrete platform/catalog/release/target contract and
must respect the current instruction to keep capability OFF.

## Main-based prerequisite composition

The first source increment starts from the fixed canonical main above and imports
only `treer-protocol/src/controller_update.rs`, its existing tests, and its public
module registration. The two imported files retain the accepted runtime source
bytes. This makes the bounded, fail-closed wire models available without adding
commands, handlers, capabilities, native entry points or dependencies. The next
increment must compose the Unix platform dependency closure and typed command
registrations against this main-based tree; the runtime integration is not yet
complete. The accepted hosted run still belongs exclusively to `ad23f3bf`.

Author validation of this prerequisite: the 12 imported controller-update model
tests pass on the main-based tree; the complete protocol package also passes
(82 unit tests and 2 integration tests). Both imported files have the same Git
blob IDs as `ad23f3bf`. `git diff --check` passes. No hosted stack, native worker,
PG matrix, browser acceptance or lifecycle was repeated for this source leaf.

## Completed main-based source composition

The candidate now includes the Unix runtime, typed command and Proxy/Auth/PG
registrations, Machine entry, focused test entries and their documentation. This
supersedes the prerequisite's remaining-composition note above. It is a source
integration, not a new hosted acceptance or production activation.

The 103 subsequently added modules, tests, scripts and research documents all
have the same Git blob IDs as accepted source `ad23f3bf`; the two protocol model
files imported in the prerequisite also retain their accepted blob IDs. A local
per-file comparison is retained in
`.treer/reviews/core222-canonical-integration-20261025/IMPORTS.json`. Existing
files were composed by updater-only hunks against `c0ebb797`:

- Unix platform registrations and `rustix` net/event features plus macOS libc.
  The lockfile only adds existing libc to platform dependencies and platform to
  Proxy dev-dependencies; no version changes or Windows bootstrap are imported.
- Controller early private worker/acceptance entry, guarded advertisement,
  asynchronous typed command dispatch and read-only operation query. The legacy
  update guard, installed-path inventory and authenticated signed metadata reuse
  hooks are retained. `launcher.rs` only widens `node_executable` to crate scope;
  canonical Rust-required launcher logic remains intact.
- Proxy catalog option, machine-scoped authenticated routes, durable tables,
  machine mutation lock reuse and pending-update no-replay reconciliation. Test
  dispatch counters remain `cfg(test)` and the real WebSocket/browser entries
  remain ignored opt-in tests.
- Console updater imports/props, Machine entry, translations and authenticated
  session epoch. Canonical prepared-Claude orchestration and App reconnect logic
  are preserved; no unrelated older Workspace tree is copied.

The default Playwright configuration excludes only
`controller-update-native.spec.ts`: that explicitly configured opt-in leaf
requires the private Core fixture binding, so selecting it in the ordinary
browser suite would be invalid. Its dedicated configuration still lists exactly
one test. This is an integration-only test-selection adjustment, not a change to
the browser acceptance implementation or product.

The only Windows-named addition is the unchanged test-only opaque envelope used
by the Unix journal's foreign-record refusal test. There is no Windows native
runtime, protected bootstrap, record-state machine or shipping capability added.
Host, host-protocol, launchers, Apps, CLI command parsing and workflows have zero
diff against the canonical base. No acceptance feature is a default feature.

Focused integration validation:

- Controller default check and feature-enabled check including tests: PASS.
- Controller feature-enabled and Proxy test-target strict Clippy: PASS.
- Protocol: 83 unit and 2 integration tests PASS, including capability-OFF and
  read-only receipt recovery gating.
- Unix platform: 64 tests PASS; 6 existing owned-subprocess fixture helpers are
  ignored by their normal test selectors.
- Web typecheck and production build: PASS; 12 observer/diagnostic mechanisms
  PASS. Three closed client browser checks PASS: normal single prepare/confirm,
  stale previous-session response isolation, and refreshed retained operation
  detail reconciliation with capability OFF. Native browser entry lists once;
  it was not executed again.
- Documentation check and diff whitespace check: PASS.

Initial `--locked` validation correctly required the two dependency-edge lockfile
updates; offline Cargo resolution made only those changes and later locked
checks pass. Missing sibling visibility for the reused Policy/audit helpers and
the test-only foreign envelope were resolved from their accepted-source hooks
before final checks. These were composition dependencies, not new runtime fixes.

The single integration handoff must bind this candidate SHA and this canonical
base. Review only the composed shared hooks and source bindings; retain the
already reviewed implementation and exact `40197073704` result as prior-source
evidence. Do not relabel that hosted run as executing the integration candidate,
restart CI/acceptance to reconstruct it, or activate an ordinary installation.

## Automatic CI follow-up: test image and CLI contract

Main `a5d6162b` automatic CI `40203031672` passed formatting and workspace
compilation, then failed in Agent Server tests. The updater fixtures digest the
running test executable (including synthetic socket-peer Host fixtures) through
the production image reader. Its 128 MiB bound also counts Linux debug sections;
an oversized test executable is not a valid fixture for that reader. The test
profile now strips only Agent Server debug information while retaining symbols
and the normal unoptimized test behavior. Development/release profiles, image
limits, image authentication and runtime code remain unchanged. This is not a
larger-image exemption or an ignored test.

The same CI run also found a stale `apps/cli-contract.json`. Regenerate it with
the real parser's `TREER_UPDATE_CLI_CONTRACT=1` test, then verify again without
that variable. This updates the snapshot only; it does not change CLI parsing.
Focused unit validation belongs to this follow-up source. The previous hosted
native/browser/PG result still belongs to `ad23f3bf`; no actual stack is rerun
for this test-build/snapshot correction.

Author validation on macOS: all 22 top-level tests named by the failing CI panic
summary pass individually, including the parent that invokes the ignored
ownership helper. The resulting test executable is 68,346,080 bytes. The CLI
contract test passes again without its generation variable; its sole snapshot
addition is the already implemented `agent admin local-inventory` command.
Formatting, documentation and diff checks pass. This does not claim a Linux
rerun, a completed workspace/PG suite or Clippy for the failed CI run; those
original results and the production image bound remain unchanged.
