import assert from "node:assert/strict";
import { test } from "node:test";
import { toBlocks } from "../src/lib/blocks";
import { diffCounts, parseDiff } from "../src/lib/diff";
import { changedFiles, liveStep, retryText, runStatus, shortPath } from "../src/lib/run";
import { activityStats, compact, duration, toolState } from "../src/lib/format";
import { safeImageUrl, safeUrl, workspaceFilePath } from "../src/lib/url";
import { reducer, shareTurns, type State } from "../src/state/store";
import { t } from "../src/i18n";
import { exportName, toMarkdown } from "../src/lib/export";
import { decideScroll, NEAR } from "../src/lib/follow";
import type { HistoryItem, Snapshot, TurnDto } from "../src/types";

const item = (id: string, kind: HistoryItem["kind"], extra: Partial<HistoryItem> = {}): HistoryItem => ({ id, kind, text: id, ...extra });

test("workspace file links map relative, absolute and line references to the existing preview API", () => {
  for (const reference of ["src/a.ts", "./src/a.ts", "src/a.ts:12", "src/a.ts:12:3", "src/a.ts#L12", "/work/project/src/a.ts", "file:///work/project/src/a.ts#L12"]) {
    assert.equal(workspaceFilePath(reference, "/work/project"), "src/a.ts", reference);
  }
  assert.equal(workspaceFilePath("special%20%23%20100%25.txt", "/work/project"), "special # 100%.txt");
  for (const reference of ["C:\\Work\\Project\\src\\a.ts:12", "c:/work/project/src/a.ts#L12", "file:///C:/Work/Project/src/a.ts#L12"]) {
    assert.equal(workspaceFilePath(reference, "\\\\?\\C:\\Work\\Project"), "src/a.ts", reference);
  }
});

test("file-link presentation never makes an outside path appear inside or accepts a remote scheme", () => {
  assert.equal(workspaceFilePath("/work/project-other/a.ts", "/work/project"), "/work/project-other/a.ts");
  assert.equal(workspaceFilePath("C:/outside/a.ts", "C:/work/project"), "C:/outside/a.ts");
  assert.equal(workspaceFilePath("../outside.txt", "/work/project"), "../outside.txt", "the server rejects traversal rather than the UI silently normalizing it");
  for (const reference of ["https://example.test/a", "mailto:a@example.test", "javascript:x", "javascript%3Ax", "data:text/html,x", "//evil.test/x", "file://evil.test/x", "#heading", "?query", "a%00.txt", "a\u0001.txt", "\\\\evil.test\\x", "%2F%2Fevil.test/x"]) {
    assert.equal(workspaceFilePath(reference, "/work/project"), undefined, reference);
  }
});

test("safeUrl keeps http(s), mailto and relative references only", () => {
  for (const ok of ["https://example.test/a?b=1", "http://example.test", "mailto:a@example.test", "docs/readme.md", "#section", "?x=1"]) assert.equal(safeUrl(ok), ok, ok);
  for (const bad of ["javascript:alert(1)", " JaVaScRiPt:alert(1)", "data:text/html,<script>", "file:///etc/passwd", "//evil.test/x", "vbscript:x", "blob:https://x/y", "\u0001javascript:x", "\\evil.test/x", "/\\evil.test/x", "", null, undefined]) assert.equal(safeUrl(bad as string | null | undefined), undefined, String(bad));
});

test("safeImageUrl allows same-origin, data raster and blob images, never svg data or remote hosts", () => {
  for (const ok of ["img/logo.png", "data:image/png;base64,iVBORw0KGgo=", "data:image/webp;base64,AAAA", "blob:https://app.test/3f2a"]) assert.equal(safeImageUrl(ok), ok, ok);
  for (const bad of ["https://example.test/x.png", "http://example.test/x.png", "//example.test/x.png", "data:image/svg+xml;base64,PHN2Zz4=", "data:image/png,notbase64<", "javascript:alert(1)", "\\evil.test/x.png", "", null]) assert.equal(safeImageUrl(bad as string | null), undefined, String(bad));
});

test("durations, compact counts and tool states", () => {
  assert.equal(duration(0), "0s"); assert.equal(duration(12_400), "12s"); assert.equal(duration(65_000), "1m 5s"); assert.equal(duration(3_720_000), "1h 2m");
  assert.equal(compact(999), "999"); assert.equal(compact(1_234), "1.2k"); assert.equal(compact(15_000), "15k");
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

test("streamed older pages replace the window, keep live status, and return to latest without a growing cache", () => {
  const start: State = { snapshot: snapshot([turn("latest", "now")], 150), connected: true, error: null, notice: null, submitting: false, loadingOlder: false, settingsBusy: false, goalBusy: false, notices: [] };
  const older = snapshot([turn("old", "before")]);
  older.history = { limit: 50, page: true, cursor: "opaque:32", totalTurns: 530 };
  let state = reducer(start, { type: "snapshot", snapshot: older, source: "http" });
  const live = snapshot([turn("latest", "changed"), turn("new", "new")]);
  live.runtime.state = "working";
  state = reducer(state, { type: "snapshot", snapshot: live, source: "ws" });
  assert.deepEqual(state.snapshot?.thread?.turns.map(t => t.id), ["old"]);
  assert.equal(state.snapshot?.runtime.state, "working");
  assert.equal(state.snapshot?.history.cursor, "opaque:32");
  const earlier = snapshot([turn("earlier", "earlier")]);
  earlier.history = { limit: 50, page: true, cursor: null, totalTurns: 530 };
  state = reducer(state, { type: "snapshot", snapshot: earlier, source: "http" });
  assert.deepEqual(state.snapshot?.thread?.turns.map(t => t.id), ["earlier"]);
  state = reducer(state, { type: "snapshot", snapshot: live, source: "http" });
  assert.deepEqual(state.snapshot?.thread?.turns.map(t => t.id), ["latest", "new"]);
  assert.equal(state.snapshot?.history.page, undefined);
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

test("changed files merge per path with line counts; diff headers are not counted", () => {
  const items = [
    item("f1", "fileChange", { changes: [{ path: "src/a.ts", kind: "update", diff: "--- a/src/a.ts\n+++ b/src/a.ts\n@@ -1,2 +1,2 @@\n-old\n+new\n keep" }, { path: "src/b.ts", kind: "add", diff: "@@ -0,0 +1,2 @@\n+one\n+two" }] }),
    item("c", "commandExecution"),
    item("f2", "fileChange", { changes: [{ path: "src/a.ts", kind: "update", diff: "@@ -5 +5,2 @@\n+more\n ctx" }] }),
  ];
  assert.deepEqual(changedFiles(items).map(({ path, kind, added, removed, diffs }) => ({ path, kind, added, removed, diffs: diffs.length })), [
    { path: "src/a.ts", kind: "update", added: 2, removed: 1, diffs: 2 },
    { path: "src/b.ts", kind: "add", added: 2, removed: 0, diffs: 1 },
  ]);
  assert.deepEqual(diffCounts("--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n+c"), { added: 2, removed: 1 });
});

test("run status is always a word, and failures and restarts never read as ready", () => {
  const base = snapshot([turn("1", "a")]);
  assert.equal(runStatus(base, true, base.thread).key, "idle");
  assert.equal(runStatus(base, false, base.thread).key, "reconnecting");
  const failed = snapshot([{ ...turn("1", "a"), status: "failed", error: "502" }]);
  assert.equal(runStatus(failed, true, failed.thread).key, "failed");
  const restarting: Snapshot = { ...base, ready: false, thread: null, runtime: { state: "starting", error: "Provider stopped", recovery: { state: "restarting", restarts: 1 } } };
  assert.deepEqual([runStatus(restarting, true, null).key, runStatus(restarting, true, null).tone], ["restarting", "warn"]);
  assert.equal(runStatus({ ...base, runtime: { state: "working", error: null } }, true, base.thread).tone, "busy");
  for (const key of ["starting", "restarting", "working", "blocked", "failed", "idle", "reconnecting"]) assert.ok(t(`status.${key}` as Parameters<typeof t>[0]).length > 0);
});

test("the live step names the running command, writing or thinking, ignoring steers", () => {
  const live = (items: HistoryItem[]): TurnDto => ({ id: "l", startedAt: null, status: "inProgress", error: null, items });
  assert.equal(liveStep(live([item("u", "userMessage")])), "Thinking");
  assert.equal(liveStep(live([item("u", "userMessage"), item("c", "commandExecution", { text: "npm test\n--watch", status: "inProgress" })])), "Running npm test");
  assert.equal(liveStep(live([item("a", "agentMessage"), item("s", "userMessage")])), "Writing");
});

test("retry re-sends only a typed message of a turn, and paths shorten to two segments", () => {
  const failed = (items: HistoryItem[]): TurnDto => ({ id: "f", startedAt: null, status: "failed", error: "x", items });
  assert.equal(retryText(failed([item("u", "userMessage", { text: "do it" })])), "do it");
  assert.equal(retryText(failed([item("u", "userMessage", { text: "[来自 /team/a，排队于 2026-10-15T00:00:00Z]\nhi" })])), null);
  assert.equal(retryText(failed([item("u", "userMessage", { text: "look", attachments: [{ id: "img_" + "0".repeat(32), mime: "image/png", size: 1 }] })])), null);
  assert.equal(shortPath("/home/me/work/project"), "…/work/project");
  assert.equal(shortPath("/work"), "/work");
});

test("a provider restart keeps the last conversation visible until a thread returns", () => {
  const start: State = { snapshot: snapshot([turn("1", "a")]), connected: true, error: null, notice: null, submitting: false, loadingOlder: false, settingsBusy: false, goalBusy: false, notices: [] };
  const down = { ...snapshot([]), ready: false, thread: null };
  let state = reducer(start, { type: "snapshot", snapshot: down, source: "ws" });
  assert.equal(state.staleThread?.turns[0]?.id, "1");
  state = reducer(state, { type: "snapshot", snapshot: down, source: "ws" });
  assert.equal(state.staleThread?.turns[0]?.id, "1", "repeated restart frames keep the copy");
  state = reducer(state, { type: "snapshot", snapshot: snapshot([turn("1", "a"), turn("2", "b")]), source: "ws" });
  assert.equal(state.staleThread, null);
});

test("only the reader scrolling up stops following; browser resets and hidden frames never leave the view at the top", () => {
  const at = (top: number, height = 40000, client = 600) => ({ top, height, client });
  const bottom = at(40000 - 600);
  const following = { pinned: true, last: bottom };
  // Regression: a hidden Console tab (display:none) or a placeholder swap resets a long conversation to the top. That
  // is not the reader: the view goes back to the latest message instead of staying at the top.
  assert.equal(decideScroll(following, at(0), false), "follow");
  assert.equal(decideScroll(following, at(0, 0, 0), false), "hidden");
  assert.equal(decideScroll(following, at(12000, 52000), false), "follow", "scroll anchoring while content changes size");
  assert.equal(decideScroll(following, at(30000, 40000, 700), false), "follow", "the frame was resized");
  // The reader's own wheel, touch, key or scrollbar input does stop following, wherever it goes.
  assert.equal(decideScroll(following, at(0), true), "unpin");
  assert.equal(decideScroll(following, at(30000), true), "unpin");
  // Find-in-page or an in-page link moves the view without input or a size change: also the reader.
  assert.equal(decideScroll(following, at(30000), false), "unpin");
  // Reaching the bottom again follows; moving down or staying put changes nothing.
  assert.equal(decideScroll({ pinned: false, last: at(1000) }, at(40000 - 600 - NEAR + 1), true), "pin");
  assert.equal(decideScroll({ pinned: false, last: at(1000) }, at(2000), true), "keep");
  // A reader who scrolled up keeps their place: a reset to the top while away restores it, layout shifts keep it.
  assert.equal(decideScroll({ pinned: false, last: at(15000) }, at(0), false), "restore");
  assert.equal(decideScroll({ pinned: false, last: at(15000) }, at(14000, 39000), false), "keep");
});
