# Supply-chain and release security review — 2026-10-12

Status: local review; workflow fixes committed in `d3844fd`, approved by core-dev
with an independent 3/3 regression run. Source baseline: `732da11` (same dependency locks as
the released `3372b34`). This is evidence and a remediation plan, not a security
certification or permission to deploy critical services.

## Scope and evidence limits

Reviewed the generated Unix bootstrap in `crates/treer-proxy/src/api/machines.rs`,
`scripts/install.ps1`, the signed publisher/Controller consumer, Compose updater,
all four GitHub workflows, dependency locks, and documented deployment controls.
Every security reproduction used temporary local files, fake programs, loopback
HTTP, or a disposable container with archived source and no host HOME, tokens,
SSH agent, Docker socket, or caller environment. No Canary/user-machine attack,
credential-store inspection, production setting change, push, deploy, or runtime
restart was performed for this review. The separately authorized preceding
Canary release is not a penetration-test result.

No confirmed P0 was established within this supply-chain scope. Findings below distinguish exploitable code
patterns, conditional trust risks, dependency scanner matches, and unverified
operational controls. A scanner's “critical” rating is not proof of a Treer P0.

## Findings, in priority order

### P1-SC1: dispatch input becomes shell code in a publishing job — fixed locally

- **Observation:** GHCR `retag` inserted `inputs.version` and `inputs.channel`
  directly into Bash source, including its supposed validation. The job has
  `packages: write` and later logs into GHCR.
- **Local reproduction:** `node --test scripts/release-security.test.mjs`
  substitutes `v1.2.3$(printf injected > "$MARKER")` into the historical check.
  It writes a marker in the fixture directory. The fixed path rejects that same
  environment value before fake Docker is called. No real token is available.
- **Impact:** arbitrary commands in that job, with potential access to its
  short-lived publishing token. An actor must already be able to dispatch the
  workflow or influence input supplied by an authorized operator; this is not
  an unauthenticated public trigger. No actual token exposure was observed.
- **Recommendation/fix:** environment data, strict version/channel/image
  allowlists, validation before registry login, and quoted argv in
  `scripts/retag-ghcr.sh`. Three regression tests also cover safe argv and
  malicious newline/quote/option values. Keep protected release refs and
  human approval as separate operational requirements.
- **Size/status:** small; implemented in `d3844fd`, cross-review approved.

### P1-SC2: bootstrap has no independent publisher authentication — design needed

- **Observation:** Unix bootstrap downloads, chmods, and moves three binaries
  without **any** SHA256 or build-identity check. PowerShell validates all three
  entries in same-origin `SHA256SUMS` and matching `--version` identities, but
  neither bootstrap authenticates an independently provisioned signing key.
  The downloaded bootstrap script itself is trusted before these checks.
- **Local reproduction:** reconstruct the Rust raw-string template, substitute
  a temporary `127.0.0.1` HTTP artifact directory, and run with fresh HOME and
  temporary install paths. Supply three text files reading `LOCAL CORRUPTED
  FIXTURE; NOT A TREER BINARY`, without a checksum file. Unix exits 0 and installs
  those files. No binary is executed and no existing installation is touched.
  PowerShell was source-reviewed; native Windows was not available locally.
- **Impact:** Unix accepts corruption; an attacker controlling a trusted
  distribution origin, or an explicitly selected insecure HTTP transport, can
  supply machine-account code. Same-origin hashes would catch corruption but
  would not stop an origin that replaces both binary and checksum. This finding
  does not establish an HTTPS/certificate bypass.
- **Recommendation:** independently authenticate the bootstrap/trust key, then
  use signed manifests for all platforms and validate the whole set before any
  execution/replacement. Add Unix checksum/identity checks as an interim
  integrity improvement, clearly labeled as insufficient origin authentication.
- **Size/status:** medium/large for the actual trust-root contract; assigned to
  core-dev with a design request. No superficial same-origin hash was advertised
  as a fix for independent authenticity.

### P1-SC3: valid signed releases can be replayed; signing-key rotation is manual

- **Observation:** the Controller verifies Ed25519 signatures, key binding,
  channel/manifest paths, byte lengths, and binary digests before execution.
  Its deserialized channel/manifest and `download()` API have no expiry,
  persistent highest release sequence, or currently installed minimum version.
  Publisher `created_at` is not freshness enforcement. This is already admitted
  by the maintained security document.
- **Local evidence:** signed-release regression tests pass for authenticated
  fixture metadata; source inspection confirms no consumer timestamp/version
  state is consulted. A real installed-machine downgrade was deliberately not
  attempted. Authentic old signatures alone cannot prove freshness.
- **Impact:** control of a distributor or cached signed channel permits replay
  of an older valid release under the same trust key. It does not permit signing
  arbitrary new binaries without the private key.
- **Recommendation:** signed monotonic release sequence and expiry, durable
  per-channel highest accepted state, explicit operator-controlled rollback,
  clock/error policy, and tested recovery. Define key revocation/rotation and
  independent fingerprint provisioning before claiming rollback resistance.
- **Size/status:** medium/large protocol and recovery change; plan only, core-dev
  notified. Missing/untrusted signatures must continue to fail closed.

### P1-SC4: publisher and container distribution remain broad trust roots

- **Observation:** the publisher creates an unencrypted PKCS#8 Ed25519 PEM with
  mode 0600 and refuses group/other-readable private keys. `publish` and `promote`
  load that key while performing network publication; offline/HSM signing and
  threshold authorization are not implemented. GHCR image update checks registry
  digests/tags but does not verify a publisher signature or provenance policy.
  The Compose updater has the host Docker socket, as already documented.
- **Local evidence:** release-key/signature tests and fake-Docker updater tests;
  source review only for key storage. No actual key file or registry/deployment
  token was inspected.
- **Impact:** compromise of the signing account can forge official updates;
  compromise of a GHCR publisher can replace channel images, including a
  privileged updater. This assumes compromise of a trusted publisher, not a
  demonstrated unauthorized registry write or updater authentication bypass.
- **Recommendation:** separate offline/HSM signing from an uploader that has
  no private key, inventory/fingerprint and rotation/revocation procedures,
  digest-pinned promotion, and signature/attestation verification before image
  application. Treat the updater as host-administration software.
- **Size/status:** medium/large with operational prerequisites; plan only.

### P1-SC5: mutable Actions could change publishing code — pinned locally

- **Observation:** every external Action used a movable major tag or `stable`
  branch. Publishing Actions run with a package-write token. Checkout also used
  default credential persistence into Git configuration.
- **Local evidence/fix:** resolved nine Action repositories' existing refs using
  their public GitHub ref APIs, peeled annotated tags, and pinned every use to
  its full 40-character commit. Tests reject future unpinned references and
  credential-persisting checkout steps. `packages: write` is now job-scoped to
  publish/merge/retag; other workflows remain contents-read-only. No deployment
  secrets, `pull_request_target`, or privileged PR checkout were found in these
  workflow definitions. Actual repository secret inventory was not inspected.
- **Impact:** moving an upstream tag could execute changed third-party code;
  no upstream compromise was observed. SHA pinning does not audit that code,
  its transitive dependencies, base images, or package-install hooks.
- **Recommendation:** retain pins, review updates, adopt workflow dependency
  lockfiles where supported, and enforce release-ref/environment approval in
  repository settings. This review did not enable or verify those settings.
- **Size/status:** small; implemented in `d3844fd`, cross-review approved.

### P2-SC6: dependency audit is absent from the release gate and has real matches

The isolated snapshot used cargo-audit 0.22.0, npm 10 from Node 22.14.0, and
pnpm 10.12.4. RustSec database commit was
`57dcc0f2192e025138907a495c1ac9a7e1e3e123`, last updated
`2026-10-12T20:06:48+02:00`. Audit commands returned nonzero for findings;
container completion does **not** mean the dependency audit passed.

| Lockfile | Result and interpretation |
| --- | --- |
| `Cargo.lock` | 7 advisories: rustls 0.23.43 (`RUSTSEC-2026-0285`, fixed >=0.23.45); quinn-proto 0.11.16 (five `RUSTSEC-2026-0338` through `0342`, all fixed >=0.11.18); rsa 0.9.10 (`RUSTSEC-2023-0071`, no patched version). One unmaintained and one yanked warning also need review. |
| `web/pnpm-lock.yaml` | 16 advisory records: 1 critical, 6 high, 6 moderate, 3 low. Includes shell-quote 1.10.0, sharp, undici, braces, source-map-js and postcss-selector-parser. |
| `apps/paper/package-lock.json` | 3 affected package records: proxy-addr critical, source-map-js high, undici high (multiple advisories). |
| `apps/mail/web/pnpm-lock.yaml` | 1 high advisory, source-map-js. |
| `launchers/agent/package-lock.json` | 1 low affected-package record: DOMPurify 3.4.14, two IN_PLACE-related advisories; >=3.4.17 fixes the snapshot. |
| `apps/email-endpoint/package-lock.json` | No advisory reported in this snapshot; not proof of absence of vulnerabilities. |

- **Local reproduction:** in a fresh credential-free container, mount only a
  `git archive` of the baseline read-only and private audit output/cache. Run
  `cargo audit --json`, `npm audit --package-lock-only --ignore-scripts --json`
  in npm projects, and `pnpm audit --json` in pnpm projects. Inspect dependency
  features with `cargo tree --locked -i PACKAGE` separately.
- **Reachability:** Linux default `cargo tree` includes rustls through reqwest,
  SQLx, NATS and WebSockets; quinn-proto and rsa print no selected inverse path
  for that configuration. Rustls' advisory explicitly says the handshake
  transcript remains authenticated: this is not an established MITM certificate
  bypass. Other targets/features were not proven safe. shell-quote's critical
  issue needs a comment-object token followed by a newline-containing token;
  current `formatCommandLine` uses string arrays and `parseCommandLine` rejects
  non-string tokens. No affected Treer execution path was established. Dev/build
  dependency advisories are not automatically deployed browser vulnerabilities.
- **Impact/recommendation:** triage exposure, upgrade compatible patches, add
  regression cases, and add scheduled plus release-time dependency auditing with
  owned, expiring exceptions. cargo-deny, Python/package-image scanning, and
  license policy are currently absent. Maintained updater Python code uses the
  standard library; no pip-audit of Python itself or its base OS was performed.
  Retired network-lab requirements are not a current production dependency audit.
- **Size/status:** patch updates usually small; ownership notifications sent to
  core-dev, web-dev, apps-dev and launcher-dev. No other owner's lockfile was
  silently changed. Reachability and remediated locks require owner follow-up.

### P2-SC7: lockfiles and metadata are not SBOM, provenance, or reproducibility

- **Observation:** Cargo `--locked`, npm `ci`, and pnpm frozen locks are present.
  Native artifacts have source/version/compiler metadata and SHA256; signed R2
  manifests bind that metadata. GHCR explicitly sets `provenance: false`.
  No published SPDX/CycloneDX SBOM, verified build attestation, independent rebuild
  comparison, or complete toolchain/base-image digest lock was found. Docker
  uses mutable `rust:1-bookworm` and other base tags; apt/apk inputs are not frozen.
- **Local reproduction:** inspect workflows, Dockerfiles, release staging and
  manifests; the artifact records do not supply an authenticated dependency
  inventory or independent source-to-binary proof. Two independent builds were
  not run; this report does not claim nondeterminism was experimentally proved.
- **Impact/recommendation:** retain existing checksum/signature checks, generate
  and sign SBOM plus build attestations, authenticate them at promotion/install,
  pin build inputs, and compare independent builds before claiming reproducibility.
  A publisher signature authenticates its claim, not the honesty of its build.
- **Size/status:** medium; design and release engineering follow-up, not a false
  claim that adding a JSON inventory alone solves supply-chain trust.

### P2-SC8: cloud-token rotation and effective isolation are unverified

- **Observation:** scripts address Railway environment `canary` and separate
  Canary/Production Worker names and URLs. Documentation specifies separate
  Railway PostgreSQL/NATS resources. Signed R2 uses shared signing authority for
  channel promotion. None of these source facts proves actual cloud RBAC,
  secret-manager storage, MFA, environment approvals, backup isolation, or a
  successful rotation drill.
- **Local evidence:** configuration/source inspection only. No Railway variable
  listing, Wrangler auth-file reading, platform secret retrieval, or cross-
  environment access attempt was performed for the security review.
- **Impact/recommendation:** one broad human/team/account token could bridge
  otherwise separate Canary/Production data planes. Use separate Railway project
  tokens scoped to the corresponding environment; use least-privilege Cloudflare
  account/zone/product resources, expiry and rotation. Workers script permissions
  must be assessed at actual account scope; different Worker names alone are not
  token isolation. Use separate production accounts/projects where needed.
- **Size/status:** small/medium operational work; cannot be certified from
  source. An authorized operator must verify scope, owners, expiry, audit records,
  revoke-old/validate-new rotation and backup separation without disclosing tokens.

## Minimum baseline before critical-service deployment

1. Run Treer/Agents under a dedicated unprivileged account or isolated VM/container;
   keep production signing/deployment/provider credentials outside Agent reach.
   Same-account workloads are trusted. Do not claim a Treer sandbox or zero trust.
2. Review/enforce workspace Policy, restrict membership and machine-exec authority,
   and avoid public ingress unless required. Put the Proxy behind verified HTTPS;
   keep PostgreSQL, NATS, Docker socket and updater HTTP private. Never expose the
   privileged updater to the internet.
3. Bootstrap from reviewed source or independently verified binaries; verify the
   trusted signing-key fingerprint out of band. Do not treat curl-to-shell or
   same-server checksum files as independent publisher authentication. Provision
   the updater trust key locally and preserve fail-closed signature verification.
4. Record the reviewed commit, artifact digests and approved update/rollback
   versions. Because freshness protection is absent, use explicit controlled
   updates and watch channel changes; do not promise unattended downgrade safety.
5. Separate Canary/Production credentials and resources; protect release refs,
   require production approval and MFA, minimize cloud permissions, and document
   credential owner/expiry/revocation. Never put tokens in argv, source, shared
   startup files or test artifacts. Rotate on exposure/personnel/permission change.
6. Resolve reachable high-impact advisories, record time-bounded exceptions with
   owners, and scan dependencies plus container OS inputs on every promotion.
   Retain pinned Actions, frozen locks and public build evidence. SBOM/provenance
   and independent reproducibility remain gaps until implemented and verified.
7. Test restoration, rollback and token rotation with isolated fixtures; protect
   database backups and audit retention. Review the maintained
   [unsupported claims](../security.md#unsupported-claims) with the operator.
   Outstanding controls are deployment prerequisites, not claims satisfied by
   this document.

## Validation and handoff

- `node --test scripts/release-security.test.mjs`: 3/3, with fake registry only.
- Signed-manifest/artifact/security regression suite: 22/22 passed.
- All workflow YAML parsed locally; shell syntax and documentation links checked.
- Additional launcher bootstrap `947504f` review: source plus 4/4 local fixture
  tests; approved to launcher-dev. Git pinning is not publisher authentication.
- Actual platform scopes/rotations, native Windows audit, Python/OS-image CVEs,
  independent builds and live-machine downgrade were not verified.
- core-dev approved the workflow fix and is adding the requested
  `docs/security.md` Unsupported Claims update in its owned security-core work.
  Dependency owners are preparing separate reviewed patches; this report's table
  intentionally preserves the original snapshot. No audit fix is deployed.

Primary references: [GitHub secure workflow use](https://docs.github.com/en/actions/reference/security/secure-use),
[build attestations and SBOM](https://docs.github.com/en/actions/concepts/security/artifact-attestations),
[Railway token scopes](https://docs.railway.com/integrations/api),
[Cloudflare permission scopes](https://developers.cloudflare.com/fundamentals/api/reference/permissions/),
[rustls advisory](https://rustsec.org/advisories/RUSTSEC-2026-0285.html),
[shell-quote maintainer advisory](https://github.com/advisories/GHSA-pqg4-j6r4-53mv).
