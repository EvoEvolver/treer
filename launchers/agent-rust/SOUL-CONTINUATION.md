# Soul continuation: terminal metadata only

The Rust Claude import accepts an explicitly captured supported idle snapshot
with up to 100 terminal background records (`completed`, `failed`, `stopped`).
It preserves the complete validated records, including order and IDs, in
`claudeTerminalTasks` in the private checkpoint and AIS `backgroundTasks`.
Combined task metadata is limited to 1 MiB. Unknown fields, duplicate IDs,
invalid types and running tasks refuse import before provider startup. This is
passive history: the background-task capability remains false. It does not
resume tasks, open their output paths or provide task controls.

Existing imports still require the exact `queue.json` SHA256. A legacy source
containing only `agent-ui.json` may instead provide **explicit**
`queueSha256: null` in its seed, only when the inventoried source really has no
queue file and the supported snapshot queue is empty. Omission refuses.
No synthetic queue file is written into the source or backup. Existing
agent/cwd/thread/idle/history and byte-backup checks remain in force. The seed
is an operator-bound supported API snapshot, not provider history or task
execution authority.

## Original Soul wrapper

This is not permission to rerun the old wrapper. Deployed Soul v1 `run` creates
a fresh incarnation; its restore script may copy session/memory. Migration
must use an operator-owned continuation wrapper binding the existing Soul
ID/incarnation, restored workspace/environment, session ID and exact launcher
image. It must not invoke `run`, download an archive or rerun the restore
script. Launcher owns that target-bound operational package; this source
change does not modify Soul or operate an installed Agent.

Rollback uses stopped-process `--restore-node-backup` and the original pinned
Node continuation entry. Old Node source
`a9989f437b385b0c8ab4d63ba35c7780b0920955`,
`apps/agent-ui/src/harness/claude.ts`, initializes an empty task map and
`loadHistory` reads only user/assistant history, not background state. Therefore
Node rollback does **not** prove task rows reappear in AIS. Preserve the
supported seed and Rust checkpoint for these records; never delete them or
claim task-view equivalence from restored source bytes. A stronger rollback
presentation requirement needs a supported adapter before activation.
Active-background migration remains unsupported.

## Focused verification

From `launchers/agent-rust`, using a separately built fixture executable:

```sh
cargo test --locked --features fixture-harness --bin treer-agent-rust-spike soul_terminal
cargo test --locked --features fixture-harness --bin treer-agent-rust-spike supported_import
RUST_SPIKE_BIN=/absolute/fixture-binary node --test \
  --test-name-pattern='receiptless Soul|actual Claude import' tests/claude-entry.mjs
```

The actual executable fixture imports 21 synthetic terminal records with no
source queue, executes one fresh same-session request, restarts, checks all
records unchanged and no replay, then restores the exact original Node file
without synthesizing a queue. It does not run the deployed Soul wrapper or
establish live Linux migration success. The initial rollback fixture omitted
Controller authentication and refused `private state unavailable`; the fixed
fixture uses the same supported private-state discovery as initial start.
No product check was relaxed.

On 2026-10-22 the final selected executable suite passed 12/12 checks, including
the 21-record continuation and refusal controls. The terminal validator and
supported-import unit checks each passed. Default and fixture-feature
all-target Clippy with `-D warnings`, documentation checks, new-module rustfmt
and diff checks passed. Entry log: `/Users/dooms/.cache/core232-soul-entry-final.log`.
The first strict Clippy run found a redundant boolean expression, corrected
without changing the predicate. Native Linux artifact construction and the
target-bound wrapper's operational acceptance remain Launcher-owned.
