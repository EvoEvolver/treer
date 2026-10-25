# First ordinary Linux Controller release: preparation proposal (#111 / #222)

Prepared against main `f4691cba911f76f8f845bf91844a938ee1fcc49d`.
This document authorizes no capability change, publication, catalog import or
enrolled lifecycle. Core owns the source/input contract; Release is the single
package and eventual execution owner. Leader collects the approval items below.

## Closed milestones and exact evidence

| Milestone | Accepted source and result |
| --- | --- |
| Browser login/preview/one confirmation, refresh/query-only recovery, real WS/native/PG same operation | `ad23f3bf`, run `40197073704` SUCCESS; operation `7036c602-4a96-46af-bbe2-4979a7cad800`; prepare/confirm 1/1, duplicate writes 0, typed sends 1/0; exact result SHA256 `73ba2baf852d450e2d1a1053aa9b23549370b1fb99ac538d3042911e074c6931` |
| Canonical integration and independent hook review | `7c2daf01`, Core2 #222:8945; accepted implementation blobs retained, current Launcher/Host/CLI hooks preserved |
| Main CI corrections and complete automatic checks | `f4691cba`, CI `40205366352` four jobs SUCCESS and Docs `40205366388` SUCCESS; Release #222:8967; result SHA256 `168f5df98623ce6381b3e34cade3b645cceab75cd7b93473fd645fe940fc99a9` |

These close source integration and their stated validation scopes. They do not
close ordinary release publication/activation. The actual stack remains attributed
to `ad23f3bf`, not main; earlier failed runs retain their original results. Do not
rerun accepted HTTPS/native/PG/manager/browser matrices for this preparation.

## One named candidate and one current source

The proposed first machine is **mariana**, Linux x86_64, verified through normal
machine discovery and `treer exec --machine mariana --cwd . --timeout 10000 --
uname -sm` on 2026-10-25. It is a candidate, not an activation selection already
approved by the user.

- Workspace: `ws_16edd11f50f74f6888b168920910d315`.
- Server: `srv_65117ce0a23e4e8488548a8118928bcd`; root `/h/28/zijian`.
- Online Controller: `0.1.14 (c7cf3b051120fe0f3a4f19cc927dd3a3a22f1546)`.
- Existing Host: `0.1.13 (1a8639b9a2abb11789ae3c69b5c15b54c232f77b)`, nohup.
- Existing listener reported by Release: `127.0.0.1:8790`.

Release #222:8972 rehashed the existing c7cf release inputs (these are publisher
inputs, not a fresh measurement of the installed process):

| File | Bytes | SHA256 |
| --- | ---: | --- |
| Controller | 26742816 | `c29799d0902105a579d8d856a777790f31bfb8afefede617a220a433bedfd90a` |
| CLI | 8076424 | `4f2d86c085a35a32ebac9fd637d09fae54e8137f0816dc8c984716caa7b46e3a` |
| SHA256SUMS | 240 | `75a7ad9bb5302c7676d6388216b229c640b56e1c2a1b5076f2e1591737df46ba` |

`https://proxy.canary.treer.ai/artifacts/linux-x86_64/` still serves those
checksum-pinned bytes, but is mutable, not an immutable release publication.
Do not substitute this URL for an approved fixed publication prefix. Alienware
is not a parallel candidate: its operator inventory/custody gap and extra
enrollments do not make it a smaller rollout. No macOS/Windows/ARM line is opened.

## The ordinary hard-OFF and bootstrap boundary

`controller_update.rs::native_accepted()` returns false. Ordinary dispatch and
advertisement therefore remain unavailable even with valid catalogs. The
test-only `native-update-acceptance` feature is not a release solution.

The running c7cf Controller predates this path. Merely publishing a new target
does not let it execute the new worker; installing ordinary f469 would add the
implementation but still leave capability OFF. There is no ready native
c7cf-to-f469 Web update to authorize today.

The smallest later source decision is confined to `native_accepted()`: accept
only `all(target_os = "linux", target_arch = "x86_64")`, after approval. Do not
enable all Unix platforms or add an environment/browser bypass. This proposal
does **not** apply that change. Its eventual source SHA and ordinary artifact
pins must be frozen before a capability-enabled package can be selected.

Two distinct operations must not be conflated:

1. One operator bootstrap of the old installation onto an approved ordinary
   source. The existing `treer-agent-server update` consumes a signed channel,
   manifest and locally provisioned public key; it is not the native Web leaf.
   A bootstrap to f469 alone is a finite cap-OFF delivery, not enablement.
2. A subsequent ordinary native update requires a newer *distinct* approved
   target relative to that bootstrap identity. No such pair is currently
   frozen. Do not manufacture a release sequence, use the same bytes twice,
   relabel a fixture, or advertise an older-source target as a real upgrade.

## Package preparation that can proceed without activation

Release has the sole handoff to reuse an exact-source artifact if one exists;
otherwise build **one linux-x86_64 ordinary f469 package**, stage it privately,
and report independent file/size/hash/source/payload identities. This is package
preparation, not another native acceptance run. No signing key, channel write,
catalog import, service call or installed switch is part of that handoff.

The maintained full `build-release-artifacts.yml` has no single-platform input;
dispatching it unchanged would start the forbidden parallel platforms. Release
must restrict its task-owned build wrapper to the existing Linux build/stage
steps and record one exclusive job acknowledgment. No automatic retry on failure.
Release completed exactly run `40208251548` SUCCESS, wrapper `d587adf3`, for this
ordinary cap-OFF package (#222:8978/8981). Do not revise its inputs or start a
second run to fold in a later payload observation. Its existing build entries were:
entries, with an exact source checkout and private target:

```sh
npm --prefix launchers/agent ci --ignore-scripts
node scripts/prepare-launcher-release.mjs --output "$PACKAGE_PAYLOAD" --source-commit f4691cba911f76f8f845bf91844a938ee1fcc49d
cargo build --locked --release -p treer-cli -p treer-agent-host -p treer-agent-server
sh scripts/stage-release-artifacts.sh linux-x86_64 "$PACKAGE_OUTPUT/linux-x86_64"
```

The wrapper must set `TREER_BUILD_COMMIT`, `TREER_LAUNCHER_PAYLOAD_DIR`,
`TREER_REQUIRE_LAUNCHER_BUNDLE=1`, a private `CARGO_TARGET_DIR`, and the stage
source directory. Paths above are wrapper-owned exclusive directories, not
operator install paths. A Host file may be included in the standard three-file
checksum document, but is never installed by this proposal.

This official portable payload does not include a native manifest automatically.
It is usable as cap-OFF bootstrap input (the legacy updater does not install a
launcher), but not as evidence that a future whole native update can successfully
install a Rust default. Do not offer it as a native-upgrade catalog target on
that evidence alone. A later enabled-source/target package needs the verified
machine-specific Rust manifest/image and matching UI closure. Its existing entry
is `launchers/agent/scripts/prepare-bundle.mjs --output ABS --source-commit SHA
--native-manifest ABS --native-sha256 SHA256`, followed by the exported
`verifyBundle(directory, commit)` in `scripts/prepare-launcher-release.mjs`.
The high-level wrapper accepts only output/source flags; do not invent an option.
The pinned manifest binds physical target paths: Release must bind those to
Mariana's verified staged native/tools/assets, with separate native source
attribution when reusing an identical Rust tree. A build-runner pathname is not
an installable target binding. The installer's no-fallback guard stays intact.
Release's ordinary package version/source probes passed for
`0.1.14 (f4691cba...)`; the proposed display label is `0.1.14-f4691cba` and target
ID `linux-x86_64-f4691cba`. Exact staged inputs are:

| File | Bytes | SHA256 |
| --- | ---: | --- |
| Controller | 34670136 | `63d738ee17166a6cdb1973bb41c38bc0b174f9b8e767d3fb7d3134525e919d27` |
| CLI | 8104704 | `2ed12d9c1f3d37b66f27f4e6407dddb14e5d9406622a0803874709fa93efa7bd` |
| Packaged Host (not installed) | 4291304 | `879c2297682544ecfdacb5846d00c39be10b73595baf2b7e5dc9aec967dfceee` |
| SHA256SUMS | — | `81b136f043630793fcfcd73ac6e8bb5ee4ba62cf1fc614b915ec1b2993b72311` |

Private receipt directory: `.treer/reviews/controller222-release-inputs-f469-20261025/`;
`ORDINARY-ACTUAL-RESULT.json` SHA256
`3f1041a68c00f245f6ed8aa007b7400212e880e3e786b5788b9d83f81a2fec8e`.
Its payload manifest is
`sha256-f430f303e0e3ad1f65d77f5a0bae23f59742bd6be79ef5e3929d249829ef3e17`;
portable `runtime_ready=true` is not Rust-default/native-update eligibility.
The package gap is closed; immutable publication, signing and installation
inventory remain open. This preparation does not promise an enabled-source SHA.

## Smallest executable next action

The next action is a **read-only Mariana installation inventory**, performed by
the existing lawful machine operator, with Release collecting only the nonsecret
receipt. The prepared contract is
`.treer/reviews/controller222-release-inputs-f469-20261025/OPERATOR-INVENTORY-CONTRACT.md`.
Use the installed Controller's `service --help` and supported `service status`
entry; exact physical argv must be filled from that inventory, not guessed from
newer source. Capture the candidate's executable/config file identities and modes,
PID/birth, shared enrollment scope, unchanged Host identity and recovery channel.
No config contents, environment, credentials or private session history are needed.
Unknown fields remain missing inputs; no lifecycle follows automatically.

After that receipt, Release can seal a cap-OFF bootstrap command using the existing
entry below. This is a command template, **not runnable until its three publisher
values and physical old executable are sealed**, and not authorization to run it:

```sh
/ABS/INSTALLED/treer-agent-server update --release-base-url APPROVED_HTTPS_BASE --channel APPROVED_CHANNEL --public-key /ABS/EXISTING/TRUSTED_PUBLIC_KEY
```

Approve at most this finite bootstrap first: c7cf to the pinned f469 Controller/CLI,
with capability still OFF and no launcher installation. Do not combine it with an
unspecified native activation. Its publication/signing custody and exact affected
installation scope are the current external inputs; no additional Core runtime
implementation or acceptance rerun is needed to describe this action.

## Exact catalog and execution inputs still to seal

Use existing schemas and routes; there is no need for another task framework,
catalog API, general downloader or arbitrary command interface.

- Proxy: `--controller-update-catalog ABS` imports `{schema_version:1,catalogs}`
  through `AuthStore::provision_controller_update_catalog`. Match the Console
  and Proxy release to the updater API/schema before exposing the Machine entry.
- Controller: existing private service config file
  `controller-update-catalog.json`, schema `{schema_version:1,revision,
  accept_shared_binaries,entries}`. Keep `accept_shared_binaries:false` for this
  first proposal. Scope must be proven exclusive; otherwise stop for a new
  explicit shared-installation decision, not silently expand the target.
- Both catalogs need exactly equal platform/target/full four-field identity and
  strictly increasing release sequences. Preserve the installed entry. The
  Controller adds the independent SUMS pin, exact HTTPS prefix ending `/`,
  `allow_pinned:true`, and explicit cross-origin consent if needed. Never derive
  trust from the bytes' own version output or fake missing digests.
- Release must obtain lawful service-account/operator custody for the physical
  Controller/CLI paths, matching config pair, private directory modes, other
  enrollments sharing either path, current image PID/birth/digests and actual
  launch-manager binding. Existing machine discovery does not establish these.
  Do not print private config, transfer credentials, spoof identity or fall back
  to an arbitrary root. Mariana previously had additional Controller processes.
- Seal a fixed publisher prefix and (for legacy bootstrap) the signed release
  version/channel, locally trusted public-key fingerprint and actual key path.
  `release-r2.mjs prepare` reads a signing key: it is not a key-free dry run and
  has not been run by Core. Native local pins are an explicit operator trust
  transition, not publisher-signature or freshness proof.
- Freeze actual catalog revision/floor and sequences only after inventorying
  existing catalog state; do not assume revision 1 or overwrite a pending job.
  Publish private catalog bytes atomically (directory 0700/file 0600), then keep
  them unchanged through a job. The Proxy import currently occurs at startup;
  it is not a live browser edit and may require a separately scoped deployment.

## One-update impact and terminal contract

The eventual native update replaces only Controller+CLI and requests one
Controller replacement through the existing Host. It preserves Host process,
registration, root, Agents and their sessions. Expect a brief control-plane
disconnect; its duration has no accepted production SLO. The worker then invokes
the embedded launcher's normal **install** and status: this can change default
launcher/profile state even though no running Agent is restarted. Review that
payload/default impact explicitly; do not describe the operation as only two
files. Custom profiles remain subject to installer refusal.

The legacy bootstrap also replaces Controller+CLI and may activate every local
registered Controller in an ordinary operator context. It cannot be approved as
single-target until installation sharing and the supported invocation context
prove that scope. Never synthesize `TREER_SERVER_ID` to shrink its scope. An
unknown/partial result retains evidence and stops; it grants no retry, queue
clear, journal edit, rollback over an unquiesced installer or second operation.

After an approved native dispatch, require one normal human preview/confirmation,
one operation UUID through native/PG/browser, complete identity+whole health,
Host/workload preservation, and query-only reconnect/refresh (duplicate writes
0). A successful bootstrap is reported separately and does not satisfy these
native postconditions. Do not repeat the isolated acceptance to simulate an
enrolled result.

## Consolidated decisions for leader/user

No approval is requested for the already authorized package preparation. Before
any real change, leader must present **one sealed summary** containing:

1. The single Mariana target and exact existing/shared installation scope;
   whether the requested finite action is cap-OFF bootstrap or later native
   activation; actual from/to source and independent binary pins.
2. Whether to authorize the Linux-x86_64-only source gate change and bootstrap
   onto that enabled source. f469 itself is not such a source. Choose a genuine
   later native release target separately; do not approve an unspecified pair.
3. Exact publisher prefix/signed channel and key fingerprint, pin/cross-origin
   trust choice, Proxy/Controller catalog bytes/revision and any matched
   Proxy/Console deployment needed to load them.
4. The Controller disconnect plus launcher default/profile impact, backup and
   no-retry/reconciliation contract, and Release as the only actual executor.

Unverified boundaries remain ordinary publication, old-Host compatibility with
this exact candidate, real installation sharing, native Rust payload closure,
production manager/cgroup behavior, independent signed-channel freshness,
macOS/Windows/ARM activation and enrolled rollout. Completed source/CI/isolated
evidence must not be relabeled as those outcomes. Until these finite inputs and
decisions are sealed, retain capability OFF and perform no installed mutation.
