# Dependency advisory query (2026-10-18)

At 06:04:06 UTC, queried all 150 registry package name/version pairs in this
crate's `Cargo.lock` against the [OSV batch API](https://google.github.io/osv.dev/post-v1-querybatch/),
using ecosystem `crates.io`. This includes target-specific registry packages,
not only the packages compiled on this Mac. Lockfile SHA-256:
`14ecfafc8d0e474c1e4241c49b743be253a2714233bddbc4e4a345f716486c00`.

The API returned 150 results, no pagination tokens, and zero vulnerability IDs.
Only public package names and versions were sent to the third-party OSV
service. OSV lists the
[Rust Advisory Database among its sources](https://google.github.io/osv.dev/data/).

This is a dated advisory-database query, not a source audit or a guarantee of
dependency safety. `cargo audit` is not installed and was not run; its additional
policy checks, database completeness, unpublished issues and code review are
outside this result. Re-query after changing the lockfile and before the pilot.
