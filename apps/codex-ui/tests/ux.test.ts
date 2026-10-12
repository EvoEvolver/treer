import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile, symlink, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { listFiles, previewFile, PREVIEW_LIMIT } from "../src/files.js";
import { mapItem, mapPlanSteps } from "../src/map.js";
import { CodexRuntime } from "../src/codex.js";

test("file browser bounds paths and symlinks, skips special entries and caps preview reads", async () => {
  const temporary = await mkdtemp(join(tmpdir(), "codex-ui-files-"));
  const root = join(temporary, "workspace");
  try {
    await mkdir(join(root, "src"), { recursive: true });
    await writeFile(join(root, "src/safe.txt"), "<script>not HTML</script>");
    await writeFile(join(root, "large.txt"), "a".repeat(PREVIEW_LIMIT + 20));
    await writeFile(join(root, "exact.txt"), "b".repeat(PREVIEW_LIMIT));
    await writeFile(join(temporary, "secret"), "outside");
    await symlink(join(temporary, "secret"), join(root, "escape"));
    await symlink(temporary, join(root, "escape-dir"));
    await symlink(join(root, "src"), join(root, "inside"));
    await symlink(join(root, "missing"), join(root, "broken"));
    for (const path of ["../secret", "src/../exact.txt", join(temporary, "secret"), "escape", "escape-dir/secret"]) {
      await assert.rejects(previewFile(root, path), /outside|traversal/);
    }
    await assert.rejects(listFiles(root, "escape-dir"), /outside/);
    await assert.rejects(listFiles(root, "src/.."), /traversal/);
    const tree = await listFiles(root);
    assert.deepEqual(tree.entries.map((entry) => entry.name), ["inside", "src", "exact.txt", "large.txt"]);
    assert.equal((await listFiles(root, "inside")).entries[0].path, "inside/safe.txt");
    assert.equal((await previewFile(root, "inside/safe.txt")).content, "<script>not HTML</script>");
    const preview = await previewFile(root, "large.txt");
    assert.equal(Buffer.byteLength(preview.content), PREVIEW_LIMIT);
    assert.equal(preview.size, PREVIEW_LIMIT + 20);
    assert.equal(preview.truncated, true);
    assert.equal((await previewFile(root, "exact.txt")).truncated, false);
    await assert.rejects(previewFile(root, "src"), /regular file/);
  } finally { await rm(temporary, { recursive: true, force: true }); }
});

test("fileChange preserves paths, change kinds and unified diffs without interpreting markup", () => {
  const diff = "--- a/a.ts\n+++ b/a.ts\n@@ -1 +1 @@\n-old\n+<script>new</script>";
  const mapped = mapItem({ type: "fileChange", changes: [
    { path: "src/a.ts", diff, kind: { type: "update", move_path: null } },
    { path: "removed.txt", diff: "-gone", kind: "delete" },
    { path: "added.txt", kind: { type: "add" } }, null, { diff: "invalid" },
  ], status: "completed" }, "turn", 1);
  assert.equal(mapped.kind, "fileChange");
  assert.deepEqual(mapped.changes, [
    { path: "src/a.ts", diff, kind: "update" },
    { path: "removed.txt", diff: "-gone", kind: "delete" },
    { path: "added.txt", diff: "", kind: "add" },
  ]);
});

test("plan and todo items map structured and textual checklists", () => {
  assert.deepEqual(mapPlanSteps("- [x] Inspect\n- [ ] Verify"), [
    { step: "Inspect", status: "completed" }, { step: "Verify", status: "pending" },
  ]);
  assert.deepEqual(mapItem({ type: "todo", todos: [{ text: "Work", status: "in_progress" }] }, "t", 0).steps,
    [{ step: "Work", status: "inProgress" }]);
  const textual = mapItem({ type: "plan", text: "- [x] Inspect\n- [ ] Verify" }, "t", 0);
  assert.equal(textual.text, "Plan", "Checklist text must not appear a second time as an explanation");
  assert.equal(textual.steps?.length, 2);
});

test("turn/plan updates become a checklist while preserving resumed messages", async () => {
  const temporary = await mkdtemp(join(tmpdir(), "codex-ui-plan-"));
  process.env.FAKE_CODEX_PLAN = "1";
  const runtime = new CodexRuntime(resolve(import.meta.dirname, "fake-codex.mjs"), temporary);
  try {
    await runtime.start();
    await new Promise((resolve) => setTimeout(resolve, 50));
    const turn = runtime.snapshot().thread!.turns[1];
    assert.equal(turn.items[0].text, "remember 1");
    assert.equal(turn.items[1].text, "recalled 1");
    assert.equal(turn.items[2].kind, "plan");
    assert.deepEqual(turn.items[2].steps, [{ step: "Tests", status: "completed" }, { step: "Screenshots", status: "inProgress" }]);
  } finally { delete process.env.FAKE_CODEX_PLAN; await runtime.stop(); await rm(temporary, { recursive: true, force: true }); }
});
