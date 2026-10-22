# Windows native entry (#232)

The Windows build runs the same Codex app-server and Claude stream-json
adapters in a native Rust PE. It uses separate platform backends for private
state and workspace reads. Installing/selecting this image and changing saved
profiles belongs to the launcher installer; this source increment changes no
running Agent, Controller, Host or installed entry.

## Supported boundary

Fresh text sessions support the existing AIS, queue, receipts, durable delivery
ACKs and restart checkpoint. The Controller must supply its existing private
state directory. Existing directories/files must have the current user's private
DACL; the launcher checks them without repairing their permissions. Retained
platform handles reject reparse points and hardlinks. An OS share-mode-zero
lease prevents a second writer and survives as a file after holder exit.
Checkpoint publication uses the platform's write-through atomic replacement;
publication failure cannot authorize dispatch or recovery. Disk-only history
remains bounded and never becomes recovery input.

Provider programs are direct native executables, selected at build time. Each
provider is assigned a nested Windows Job with kill-on-close, distinct from the
launcher's inherited Controller/Host Job. Assignment failure refuses startup.
Assignment happens immediately after spawn, so this is not a claim of containment
before the provider's first instruction. The native test observes an owned
descendant created after assignment and checks its creation identity disappears
when the Job is closed. Windows exit diagnostics retain exit code and null Unix
signal; they do not manufacture a signal or prove arbitrary detached processes
are quiescent.

Workspace preview/download uses the retained Windows `machine_file::Root`.
Directory enumeration returns explicit HTTP 501 until retained enumeration is
available. Images are unavailable and advertised false; nonempty image requests
and stored image references refuse. These limits do not prevent text entry.

Legacy Node import/backup restore is explicitly unsupported on Windows. A
legacy per-Agent source directory or explicit import flag refuses rather than
discarding state. This increment therefore supplies new default entry, not
migration of an existing Windows Node Agent. The dispatcher must select Rust
before creating Node state, provider override environment, or an existing port
file. It supplies `AGENT_UI_PUBLIC_DIR` pointing to the bundled public assets.

## Build and binding

Build in the standalone `launchers/agent-rust` workspace with its committed
lockfile. `TREER_SPIKE_BUILD_CODEX`, `TREER_SPIKE_BUILD_TREER` and optional
`TREER_SPIKE_BUILD_CLAUDE` identify actual native build inputs. Windows-native
default builds require PE headers; script interpreters are not substituted.
`TREER_SPIKE_TARGET_CODEX`, `TREER_SPIKE_TARGET_TREER` and optional
`TREER_SPIKE_TARGET_CLAUDE` bind deployment paths separately. Windows deployment
paths must be normalized absolute drive paths ending in `.exe`; relative paths,
UNC paths, parent segments, streams and trailing aliases refuse. Record actual
input and output hashes in the artifact manifest. Runtime override variables
remain forbidden in default builds.

A missing native Claude build input leaves Claude explicitly unavailable; a
synthetic producer test does not establish an installed Claude provider or its
authentication. Cross-target `cargo check` establishes compilation only, not a
Windows artifact or native test result.

## Focused actual Windows checks

Keep default and `fixture-harness` target directories separate; never ship the
fixture image. Pin `TREER_WINDOWS_TEST_NODE` to the actual native Node executable
for the synthetic provider/Job test only. After building the fixture-feature
release binary, run these filters with `cargo test --locked --release --features
fixture-harness --bin treer-agent-rust-spike <filter>`:

- `store::tests` checks real DACL refusal, exclusive child-process lease, bounded
  checkpoint publication and invalid inputs.
- `disk_history::tests` checks bounded passive history and unsafe-file refusal.
- `files::tests` checks actual confined reads and explicit unsupported behavior.
- `attachments::tests` checks text-only behavior and image refusal.
- `rpc::tests::windows_provider_job_terminates_owned_descendant` checks Job cleanup.

Set `RUST_SPIKE_BIN` to the fixture-feature PE and run
`node --test tests/windows-native-entry.mjs`. It executes only on Windows,
creates fresh owned state, restricts that state's DACL, starts both protocol
adapters against synthetic native-Node producers, sends text, checks a second
writer refuses, and restarts from the durable checkpoint. It reads no provider
history/config/auth and touches no enrolled Agent. Cleanup terminates only its
own child handles and removes only its fresh temporary root. The subprocess
restart uses Windows termination, testing lease/Job cleanup on abrupt exit.
Startup refusal is reported immediately with the child exit/spawn status and at
most 4096 bytes of synthetic-child stderr, with the owned temporary root
redacted. This diagnostic is restricted to the suite's credential-free
environment and synthetic producers, not installed provider sessions.

Default-feature artifact creation and actual Windows installation remain distinct
results, bound to their exact source, provider paths and bytes by Release. No
macOS cross-check or synthetic provider fixture should be labeled that result.
