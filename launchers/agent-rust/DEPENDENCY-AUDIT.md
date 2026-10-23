# Dependency advisory query (2026-10-18)

At 16:49:25 UTC, queried all 150 registry package name/version pairs in this
crate's `Cargo.lock` against the [OSV batch API](https://google.github.io/osv.dev/post-v1-querybatch/),
using ecosystem `crates.io`. This includes target-specific registry packages,
not only the packages compiled on this Mac. Lockfile SHA-256:
`9c1493acdedabe13b486aa60e5aa433992bf2292e3360de96e57b876ad83d2b7`.
The transport increment makes the already-locked `hyper` and `hyper-util`
packages direct dependencies; no registry package version was changed.

The API returned 150 results, no pagination tokens, and zero vulnerability IDs.
Only public package names and versions were sent to the third-party OSV
service. OSV lists the
[Rust Advisory Database among its sources](https://google.github.io/osv.dev/data/).

This is a dated advisory-database query, not a source audit or a guarantee of
dependency safety. `cargo audit` is not installed and was not run; its additional
policy checks, database completeness, unpublished issues and code review are
outside this result. Re-query after changing the lockfile and before the pilot.
