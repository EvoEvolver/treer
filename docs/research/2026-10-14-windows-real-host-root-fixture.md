# Native Windows real-Host fixture root mismatch

Candidate `1a512715a935b49f262ad957ddf344bc5a8944eb` failed native run
38488488649, job 115607816971, in the isolated real-Host wrapper. The first
`exec_machine(cwd=".")` returned `invalid_machine_path` in 0.70s. No command
was spawned: this is separate from the preceding PowerShell timing issue.

## Cause and scope

The service fixture constructs its root as `temp_dir()/treer-windows-host-UUID`,
creates it, then passes the **raw** PathBuf into both its saved service config
and an in-process `ControllerRuntime::from_sync`. `resolve_machine_directory`
canonicalizes `root/requested` and compares it to the stored root. On Windows,
canonicalization adds a verbatim prefix and can resolve short path components;
the raw and canonical PathBuf representations need not match even for `.`.
The fixture bypasses production startup's required normalization.

The production `run_server` in main.rs canonicalizes `args.root` before building
ControllerConfig. The standalone CLI real-Controller tests in the failed job
passed 2/2 before this fixture ran; leader also reports normal `cwd="."` exec
on win_native. These are consistent with a fixture construction error, not
evidence of a production path-boundary regression. We do not loosen
`starts_with`, strip prefixes, case-fold paths or follow additional junctions.
The native rerun must confirm the exact raw/canonical relationship through
booleans; logs intentionally contain no actual path values.

`8892dfc` is an ancestor of the candidate, but modifies only Proxy bootstrap
command rendering/tests and operator documentation. It cannot alter this
manually constructed fixture root or Controller directory resolution.
The expanded fixture and in-process exec/App/Agent matrix were introduced in
`f773f6d`. The older successful `121909b` real-Host test did not construct an
in-process Controller to run this matrix. In the current candidate sequence,
042752b run 38476192186 and e3ed8ea run 38481174982 stopped at the earlier PS
gate; neither reached this newly expanded real-Host test. The 1a51271 run
passed that gate and first exposed this later assertion in that sequence.

## Minimal repair and validation

Canonicalize the created root before saving fixture configs or constructing its
in-process Controller, exactly as production startup does. Assert canonical
`.` is an existing directory within that root; emit only raw-equals-canonical,
raw-contains-canonical-cwd and canonical-contains-cwd booleans. The existing
real subprocess matrix still requires successful exec/App/Agent commands and
detached restart/stop semantics; no expectation is removed. Add an explicit
`cwd=".."` rejection before the matrix so canonical root normalization cannot
silently authorize escape.

This patch changes only test code and this report, not production behavior.
Native confirmation requires freshly built sibling Controller/Host binaries
and the exact ignored wrapper. A macOS Clippy run cannot execute or validate
the Windows-only branch. Run only the isolated wrapper, never service commands
against an existing user enrollment or managed Agent. No user-machine service
restart is part of this repair. Later failures must be investigated separately;
repairing the root does not pre-approve the remaining native matrix or release.

Native validation run 38489485150 confirmed raw-equals-canonical=false,
raw-contains-canonical-cwd=false and canonical-contains-cwd=true. Parent traversal
was rejected and the first exec/Agent/App Node matrix ran. The next PS exec
failed with exit 1 after a 32.10s overall run: a separate hosted-image module
slow-path blocker, not recurrence of root rejection. The original assertion
did not record timed_out, so duration alone is not proof of that flag.

Leader chose a second test-only repair: use Console/CLR for the real-Host PS
smoke without Utility-module auto-loading, keeping PS version, OS baseline,
system-module presence, CLR RNG, exit 0 and the original 30s limit. Explicit
timeout and duration checks are added. Controller-level compensated tests and
uncompensated Windows 11 evidence retain the original cmdlet regression.
No production binary hook is added. See quality.md for the coverage distinction;
new native confirmation is still required and no full release pass is claimed.
