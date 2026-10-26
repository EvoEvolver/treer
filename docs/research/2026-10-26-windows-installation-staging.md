# Saved Windows installation and signed staging (#111)

The new internal `Preparation::local` composes the existing saved-service format
with the sustained receiver coordinator. It resolves the normal configuration
root, reads minimal Controller/Host projections, selects exact workspace/server,
checks the loaded runtime's machine root and retains real configuration and
image handles. No current executable, PATH search, caller-selected installation,
or arbitrary private same-volume directory substitutes for saved configuration.

Physical Controller/CLI identities produce the actual affected-server list.
The existing catalog shared-installation permission and the caller's exact
already-confirmed list must agree; neither a public identity nor a bool alone
proves installation selection or upstream Policy. Existing catalog files and
saved configuration snapshots stay read-pinned through preparation and returned
custody. Config writers can temporarily be refused. Name-set checks at phase
boundaries detect changes permanently, but cannot freeze new registrations
between observations; a global enrollment CAS remains future work.

Normal local UpdateArgs select official/local/explicit self-host trust. Both
signatures and full artifact hashes are verified before a fresh EXCL operation
stage is created under the SAME runtime-owned configured startup-state root.
After flush and strict pinned reopening, confirmed admission verifies the whole
held files again before claim/reservation. Partial stages and claims remain on
error, never automatically retried, adopted, overwritten or cleaned up.

The complete finite success chain is saved installation → shared confirmation →
normal trust → signed staging → existing actual holding receiver → inspection →
explicit close plus original Child zero exit → return original reservation AND
saved-installation/configuration guards. This does not release sender custody,
retire a claim, reset an attempt, authorize mutation or infer permission from
creator exit. Unknown owner destruction retains the existing best-effort Child
teardown limitation; no survival or guard-continuity proof follows from Drop.

Validation is scoped to new projection/selection/staging controls and private
same-source dev receiver fixtures. No old native selector, matrix, live Host,
installed Controller/Agent, publisher or capability is exercised. Native results
must be recorded against the exact source rather than inherited from prior leaves.

Author checks on source `47d6f736a34e7e3a75dd16a0b0e7b5828ee84321` passed:
the new portable projection test, strict Server Clippy (including tests and the
acceptance feature), Windows platform compile/Clippy, formatting and docs.
Release's one independent source review found no scoped blocker (#111:9810).
The sole native run [40399128889](https://github.com/EvoEvolver/treer/actions/runs/40399128889)
used wrapper `42e2b567e67ffcfc30dc0a0cf39115daf43147b4`, Windows 2022,
Rust 1.90 and fixed NTFS image `20261022.344.1`. Same-source dev build, JSON test
build and direct test exits were all zero: `windows_update_installation_stage_`
ran exactly four tests (one portable, three native), all passed, none ignored.
Both dev and test executables retained their original sizes and SHA256 hashes.
Only the test process received isolated TEMP/TMP and matching LOCALAPPDATA/catalog
markers; the fixture child was not precreated. The task root had no remaining
entries and was removed. Core independently read RESULT/platform/tests receipts;
RESULT SHA256 is
`98282eb1a74cee1456e219cdda471ac24b888d4f41ee1faf1f28395fd2d77f65`.

The native success case used saved shared-installation configuration, normal
self-host trust and the actual same-source holding receiver, then returned the
original reservation and configuration guards after explicit close. The other
native cases rejected eight classes of bad scope/consent/trust/input/configuration
before staging and permanently rejected an observed inventory change and stage
replay. Installed OLD bytes stayed unchanged. These are task-private fixtures,
not live installation or complete transaction acceptance. No prior native
selector or platform matrix was rerun.

Remaining full transaction paths: adapt network release downloads to this
already-fetched signed-input seam; connect authenticated Policy and confirmation
to public dispatch without enabling it prematurely; authorize worker mutation
and lifetime; startup/fence CAS; publication, selected Controller-only restart
and health; whole-pair runtime rollback; durable outcome and safe claim retirement.
Current saved-config matching is source-derived and isolated-fixture verified,
not a field attestation of an old deployed Host's resolved configuration.
