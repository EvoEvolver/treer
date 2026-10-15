import assert from "node:assert/strict";
import { test } from "node:test";
import { toBlocks } from "../src/lib/blocks";
import { parseDiff } from "../src/lib/diff";
import { activityStats, compact, duration, plural, toolState } from "../src/lib/format";
import { safeImageUrl, safeUrl } from "../src/lib/url";
import { reducer, shareTurns, type State } from "../src/state/store";
import { t } from "../src/i18n";
import { exportName, toMarkdown } from "../src/lib/export";
import type { HistoryItem, Snapshot, TurnDto } from "../src/types";

const item = (id: string, kind: HistoryItem["kind"], extra: Partial<HistoryItem> = {}): HistoryItem => ({ id, kind, text: id, ...extra });

test("safeUrl keeps http(s), mailto and relative references only", () => {
  for (const ok of ["https://example.test/a?b=1", "http://example.test", "mailto:a@example.test", "docs/readme.md", "#section", "?x=1"]) assert.equal(safeUrl(ok), ok, ok);
  for (const bad of ["javascript:alert(1)", " JaVaScRiPt:alert(1)", "data:text/html,<script>", "file:///etc/passwd", "//evil.test/x", "vbscript:x", "blob:https://x/y", "\u0001javascript:x", "", null, undefined]) assert.equal(safeUrl(bad as string | null | undefined), undefined, String(bad));
});

test("safeImageUrl allows same-origin, data raster and blob images, never svg data or remote hosts", () => {
  for (const ok of ["img/logo.png", "data:image/png;base64,iVBORw0KGgo=", "data:image/webp;base64,AAAA", "blob:https://app.test/3f2a"]) assert.equal(safeImageUrl(ok), ok, ok);
  for (const bad of ["https://example.test/x.png", "http://example.test/x.png", "//example.test/x.png", "data:image/svg+xml;base64,PHN2Zz4=", "data:image/png,notbase64<", "javascript:alert(1)", "", null]) assert.equal(safeImageUrl(bad as string | null), undefined, String(bad));
});

test("durations, compact counts, plurals and tool states", () => {
  assert.equal(duration(0), "0s"); assert.equal(duration(12_400), "12s"); assert.equal(duration(65_000), "1m 5s"); assert.equal(duration(3_720_000), "1h 2m");
  assert.equal(compact(999), "999"); assert.equal(compact(1_234), "1.2k"); assert.equal(compact(15_000), "15k");
  assert.equal(plural("command", 1), "1 command"); assert.equal(plural("command", 3), "3 commands"); assert.equal(plural("change", 2), "2 file changes");
  assert.equal(toolState("completed"), "done"); assert.equal(toolState(null), "done"); assert.equal(toolState("failed"), "failed"); assert.equal(toolState("declined"), "failed"); assert.equal(toolState("inProgress"), "running");
  assert.equal(t("composer.placeholder", { name: "Codex" }), "Message Codex");
});

test("activity runs fold into groups while messages and plans stay visible", () => {
  const items = [item("u", "userMessage"), item("c1", "commandExecution"), item("c2", "commandExecution", { status: "failed" }), item("p", "plan"), item("f", "fileChange"), item("a", "agentMessage")];
  const blocks = toBlocks(items);
  assert.deepEqual(blocks.map((block) => block.type), ["message", "group", "plan", "group", "message"]);
  const first = blocks[1];
  assert.ok(first?.type === "group" && first.items.length === 2);
  assert.deepEqual(activityStats(items.filter((entry) => ["commandExecution", "fileChange"].includes(entry.kind))), { commands: 2, changes: 1, tools: 0, steps: 0, failed: 1 });
});

test("unified diffs become signed rows with line numbers; file headers only count before the first hunk", () => {
  const rows = parseDiff("--- a/x.ts\n+++ b/x.ts\n@@ -3,2 +3,2 @@\n keep\n--- not a header\n+added");
  assert.deepEqual(rows.map((row) => row.kind), ["header", "header", "hunk", "ctx", "del", "add"]);
  assert.equal(rows[3]?.gutter, "3"); assert.equal(rows[4]?.text, "-- not a header"); assert.equal(rows[4]?.gutter, "−"); assert.equal(rows[5]?.gutter, "+");
});

const turn = (id: string, text: string): TurnDto => ({ id, startedAt: null, status: "completed", error: null, items: [item(`${id}-a`, "agentMessage", { text })] });
const snapshot = (turns: TurnDto[], limit = 50): Snapshot => ({ ready: true, runtime: { state: "idle", error: null }, harness: { id: "codex", name: "Codex", capabilities: { steer: true, backgroundTasks: false, goal: null } },
  thread: { id: "t", title: "T", cwd: "/w", model: null, reasoningEffort: null, status: "idle", activeTurnId: null, lastError: null, createdAt: "", updatedAt: "", turns, hasOlderItems: false }, modelOptions: [], backgroundTasks: [], queue: [], goal: null, history: { limit } });

test("unchanged turns keep their identity across snapshots; changed turns do not", () => {
  const before = snapshot([turn("1", "one"), turn("2", "two")]);
  const after = shareTurns(before, snapshot([turn("1", "one"), turn("2", "two!"), turn("3", "three")]));
  assert.equal(after.thread?.turns[0], before.thread?.turns[0]);
  assert.notEqual(after.thread?.turns[1], before.thread?.turns[1]);
  assert.equal(after.thread?.turns.length, 3);
});

test("a late HTTP snapshot never shrinks a history window the reader widened", () => {
  const start: State = { snapshot: snapshot([turn("1", "a")], 150), connected: true, error: null, notice: null, submitting: false, loadingOlder: false, settingsBusy: false, goalBusy: false, notices: [] };
  assert.equal(reducer(start, { type: "snapshot", snapshot: snapshot([turn("1", "a")], 50), source: "http" }), start);
  assert.equal(reducer(start, { type: "snapshot", snapshot: snapshot([turn("1", "a")], 50), source: "ws" }).snapshot?.history.limit, 50);
});

test("the Markdown export covers messages, activity, diffs and plans with safe fences", () => {
  const turns: TurnDto[] = [{ id: "1", startedAt: null, status: "completed", error: "stopped", items: [
    item("u", "userMessage", { text: "[来自 /team/a，排队于 2026-10-15T00:00:00Z]\nhello" }),
    item("c", "commandExecution", { text: "npm test", status: "completed" }),
    item("f", "fileChange", { text: "File change", changes: [{ path: "a.ts", kind: "update", diff: "@@ -1 +1 @@\n-a\n+b\n```" }] }),
    item("p", "plan", { text: "Plan", steps: [{ step: "One", status: "completed" }, { step: "Two", status: "inProgress" }] }),
    item("a", "agentMessage", { text: "done" }),
  ] }];
  const markdown = toMarkdown(snapshot(turns));
  for (const expected of ["# T", "## From /team/a", "hello", "$ npm test", "- [x] One", "- [ ] Two (in progress)", "> Error: stopped", "done"]) assert.ok(markdown.includes(expected), expected);
  assert.ok(markdown.includes("````diff"), "a diff containing a fence is wrapped in a longer fence");
  assert.match(exportName(snapshot(turns), new Date("2026-10-15T12:00:00Z")), /^t-2026-10-15\.md$/);
});
