# Native default entry

The `treer-agent` dispatcher selects the pinned Rust launcher for Codex and
Claude. Selecting `--harness acp` explicitly selects the existing Node ACP
adapter; its workspace-global profile argv do not change. `--runtime node` is
also an explicit Node entry. A failed or missing Rust binding never falls back
to Node. Running Agents keep their copied startup and immutable assets.

`treer-agent update` forwards only to the Controller's guarded
`launcher update --require-runtime rust`; it never calls an older unguarded
update as fallback. A Controller without that flag or without a verified
embedded Rust payload refuses. `treer-agent update --bundle ABS --sha256 HASH`
uses the existing local prepared-bundle installer instead. Both routes require
a Rust default with at least one available Codex/Claude provider and leave
running sessions untouched. The selected source is explicit/local, not a
network latest-version feed. See the README's update section for discovery,
version output, root binding and platform limits.

Each prepared payload carries a machine-specific `native-runtime.json`:

```json
{
  "schema": "treer.native-launcher/v1",
  "platform": "linux",
  "arch": "x64",
  "source_commit": "<actual native source full SHA>",
  "executable": {"path": "/physical/runtime", "bytes": 1, "sha256": "<SHA256>"},
  "public_dir": "/physical/runtime-assets/public",
  "assets": [{"path": "/physical/runtime-assets/public/index.html", "bytes": 1, "sha256": "<SHA256>"}],
  "tools": {
    "treer": {"path": "/physical/treer", "bytes": 1, "sha256": "<SHA256>", "version": "<version>"},
    "codex": {"path": "/physical/native-codex", "bytes": 1, "sha256": "<SHA256>", "version": "<version>"}
  }
}
```

The abbreviated example must include every actual public asset, including
`index.html`, `boot.js`, `ui.js`, and `ui.css`. Bind `tools.claude` when that
native provider is installed. A missing provider is reported as
`unavailable-provider`, and that harness refuses startup. It is not a Node
fallback. All executable paths must match the native binary's compiled tool
bindings, not a PATH wrapper. The operator attests those build inputs and the
native platform/ABI/entry evidence; the JSON alone does not prove their origin.

Installation/status/launch verify physical paths, platform/architecture, byte
counts, SHA256 and native image headers. Hashing uses a 1 MiB buffer. The
manifest is part of the content-addressed payload and must not be edited after
activation. Native assets remain at their immutable physical paths. Same-user
executables remain trusted, as with existing launcher installation.

Prepare using the independently recorded native manifest SHA256:

```sh
node scripts/prepare-bundle.mjs --output /absolute/prepared \
  --source-commit FULL_SOURCE_SHA \
  --native-manifest /absolute/native-runtime.json --native-sha256 SHA256
node scripts/bundle-launcher.mjs update --treer /absolute/treer \
  --bundle /absolute/prepared --sha256 PREPARED_MANIFEST_SHA256
node scripts/bundle-launcher.mjs status --treer /absolute/treer
```

`status.current_runtime` distinguishes Rust, legacy Node, invalid bindings and
unconfigured native defaults, and names unavailable providers. The installer
refuses a new Codex/Claude default without a native binding. A source install
accepts the same `--native-manifest` and `--native-sha256` arguments. Rollback
uses the existing bundle installer with the exact saved previous version and
profile receipt; old physical payloads/providers must remain installed. Default
installation does not restart, migrate, clear or replay any running session.

Resuming an existing Claude session on a new journal requires the supported
checkpoint importer and its stopped-state backup contract. Passing a session ID
without that checkpoint is not a supported migration shortcut. The native
runtime's capability manifest remains authoritative: this default selection
does not add Node goals, image support, background execution, busy steering,
session transfer or unsupported provider adapters. Windows native installation
requires its own accepted Windows image/entry; neither a Mac/Linux image nor a
Node portable payload establishes Windows Rust support.
