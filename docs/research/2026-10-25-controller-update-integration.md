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
