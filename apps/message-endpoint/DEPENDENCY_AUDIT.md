# Dependency advisory audit (#159)

Local check on 2026-10-18 UTC (2026-10-17 Toronto); not a deployment or a claim
that dependency code is free of vulnerabilities. Only this independent App's
lockfiles were changed/evaluated. The root Core workspace has a separate owner.

## Rust

Built `cargo-audit 0.22.2 --locked --no-default-features` into a private temporary
installation with a fresh Cargo home, stripped environment and two build jobs.
No global executable installation or shared build target. Downloaded the public
RustSec advisory database into that private directory. Initial database commit
`999ec89d5d3f10f20be14750ec8c2d8b44e0ac9b`, updated
`2026-10-17T17:23:59+02:00`, 1338 advisories.

Initial `cargo audit --file apps/message-endpoint/Cargo.lock --json` returned
exit 1 with four matches against transitive `rustls 0.23.45`:

| Advisory | Relevant scope; matching a lockfile is not proof of App exploitability |
| --- | --- |
| [RUSTSEC-2026-0346](https://rustsec.org/advisories/RUSTSEC-2026-0346.html) | Custom client-session-store Debug output can expose session secrets. This App does not implement a custom session store or log its values. |
| [RUSTSEC-2026-0347](https://rustsec.org/advisories/RUSTSEC-2026-0347.html) | Particular ECH rejection and pre-handshake buffered-write configuration. This App does not configure ECH. |
| [RUSTSEC-2026-0348](https://rustsec.org/advisories/RUSTSEC-2026-0348.html) | Non-default Brotli certificate decompression. This App does not enable that feature. |
| [RUSTSEC-2026-0349](https://rustsec.org/advisories/RUSTSEC-2026-0349.html) | Key-share prediction/session-store behavior can weaken post-quantum confidentiality under the stated conditions; not an authentication bypass. No exploit against this App is claimed. |

All four list `>=0.23.46` as patched. Updated only `rustls` to `0.23.46` in
this App's Cargo.lock, without ignoring advisories or changing the TLS origin,
certificate-validation, redirect or proxy policy. The same database, rechecked
with `--no-fetch`, reports **exit 0, zero vulnerability matches, no warnings**.
The no-fetch JSON omits Git metadata; the database directory was the same
unchanged private checkout. Raw before/after results:
`/tmp/endpoint-159-cargo-audit.json` and
`/tmp/endpoint-159-cargo-audit-patched.json`. These local files are evidence paths,
not required deployment inputs or committed generated artifacts.

Repeat before release with a freshly fetched RustSec database; a frozen database
does not detect future disclosures. Use the independently installed audit tool,
this App's lockfile, a private Cargo/database directory and no inherited Agent
credentials. Ordinary offline App tests do not silently fetch advisories.

## Frontend build dependencies

`npm audit --package-lock-only --ignore-scripts --json` against the public npm
registry, using private HOME/cache and separate empty user/global npmrc files,
reported **exit 0, zero known vulnerabilities** (34 total dependencies).
Raw local result: `/tmp/endpoint-159-npm-audit.json`. No dependency versions were
changed by this check, and no npm lifecycle scripts were executed by the audit.

The normal frontend build/check and Rust checks are separate validation. Advisory
matching does not attest all dependency source/build scripts, reproducible
builds, malicious-package detection or runtime isolation. Release must still
verify the actual immutable artifacts and the required Policy/OS boundary.
