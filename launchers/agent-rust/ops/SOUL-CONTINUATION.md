# Continue an existing legacy Soul incarnation

`soul-continue.py` is a target-bound operational entry for the already-restored
legacy generic Soul case. It never calls Soul `run`, downloads an archive,
executes `restore.sh`, copies session/memory files, or discovers provider history.
The original startup and its failed/successful attempt evidence remain retained.

Old Soul v1 creates another incarnation on every `run`; its archived restore
script can overwrite a workspace's session/memory. Repeating that startup is
therefore not an honest same-state rollback. Inventory the current process's
nonsecret Soul selectors and exact restored script/physical launcher first.
Bind the existing Soul ID/root/restore file, Agent/server/workspace, HOME, cwd,
session and native image hashes in a new private0600 manifest. Do not select an
incarnation by modification time. No credentials belong in this manifest.

Three explicit modes select one imported Rust start, ordinary Rust continuation,
or the original physical Node entry with the same session/cwd/name. The wrapper
preserves Controller-issued environment, overlays only the recorded nonsecret
selectors/settings, and execs the selected pinned image. Identity/path/ownership,
symlink, restore hash, image hash or session mismatches refuse before exec.
Use an isolated Python interpreter and independently verify the wrapper and
manifest seal before each operation. `--verify` performs no exec or lifecycle.

This does not itself make a target eligible: the migration helper still needs a
natural checkpoint, supported snapshot/full receipt or explicitly receiptless
source proof, stopped-process/group/holder proof, exact source backup, importer
and terminal-task compatibility, same-session after checks, and one rollback
contract. The Node rollback entry does not promise restoration of volatile
mapped IDs or terminal task records unless the owning adapter proves that
separately. Keep their supported snapshot evidence and report such differences;
never manufacture pending receipts or replay tasks to recreate a view.

`python3 -I -B ops/test_soul_continue.py` runs8 actual wrapper entry controls with
synthetic executable images, including imported/final Rust selection, inherited
environment, original Node continuation and changed identity/script/image/session
or symlink refusal. These tests are not live Soul/provider migration acceptance.
