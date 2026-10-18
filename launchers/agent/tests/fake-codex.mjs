#!/usr/bin/env node
import readline from "node:readline";
import assert from "node:assert/strict";
import { appendFileSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
const goalFile = join(process.cwd(), ".fake-codex-goal.json");
let goal = null;
try { goal = JSON.parse(readFileSync(goalFile, "utf8")); } catch { /* no goal */ }

const turns = [0, 1].map((n) => ({
  id: `turn-${n}`, status: "completed", items: [
    { id: `u-${n}`, type: "userMessage", content: [{ type: "text", text: `remember ${n}` }] },
    { id: `a-${n}`, type: "agentMessage", text: `recalled ${n}` },
  ],
}));
if (process.env.FAKE_CODEX_IMAGES) {
  try { turns.push(...JSON.parse(readFileSync(join(process.cwd(), ".fake-image-turns.json"), "utf8"))); } catch { /* new sealed image fixture */ }
}
let threadId = `new-${process.pid}`;
let relayTurn = 0;
let completedTurns = 0, pendingCompletion;
let recoveryRead = false;
if (process.env.FAKE_CODEX_OLD_ERROR) {
  turns[0].status = "failed";
  turns[0].error = { message: "Historical 502 Bad Gateway" };
}
// A turn cut off by a restart is left "inProgress" in the persisted history.
if (process.env.FAKE_CODEX_STALE) turns[0].status = "inProgress";
if (process.env.FAKE_CODEX_LIVE) turns.push({ id: "turn-live", status: "inProgress", items: [{ id: "u-live", type: "userMessage", content: [{ type: "text", text: "working" }] }] });
if (process.env.FAKE_CODEX_UX) {
  turns[1].items.push(
    { id: "plan-1", type: "plan", plan: [{ step: "Inspect the workspace", status: "completed" }, { step: "Improve the file browser", status: "in_progress" }, { step: "Verify desktop and mobile", status: "pending" }] },
    { id: "change-1", type: "fileChange", status: "completed", changes: [
      { path: "src/greeting.ts", kind: { type: "update" }, diff: "--- a/src/greeting.ts\n+++ b/src/greeting.ts\n@@ -1 +1 @@\n-export const greeting = 'Hello';\n+export const greeting = 'Hello, Treer';" },
      { path: "README.md", kind: { type: "add" }, diff: "--- /dev/null\n+++ b/README.md\n@@ -0,0 +1,2 @@\n+# Workspace\n+Small tools, thoughtful interfaces." },
    ] },
    { id: "answer-1", type: "agentMessage", text: "The file browser is ready. You can explore **src/** and preview files without leaving this conversation.\n\nThe changes below keep everything read-only." },
  );
}
// Rich fixtures for the React UI: reasoning, passing and failing commands, hostile markdown, and a long history for paging.
if (process.env.FAKE_CODEX_RICH) {
  turns[1].items.push(
    { id: "think-1", type: "reasoning", summary: ["Compare the storage options"], text: "" },
    { id: "cmd-1", type: "commandExecution", command: "npm test", aggregatedOutput: "ok 1 - queue\nok 2 - store", status: "completed" },
    { id: "cmd-2", type: "commandExecution", command: "node --test tests/durable-queue.test.ts", aggregatedOutput: "not ok 1 - fixture path", status: "failed" },
    { id: "rich-1", type: "agentMessage", text: "## Result\n\nSee [the docs](https://example.test/docs) and [bad](javascript:window.xss=3), [ftp](ftp://example.test/x), [tel](tel:123), [slash](/\\\\evil.test/x).<form><input name=\"x\"><button>go</button></form>\n\n<img src=x onerror=\"window.xss=1\">\n\n<svg onload=\"window.xss=2\"></svg><script>window.xss=4</script>\n\n![remote](https://example.test/x.png)\n\n- one\n- two\n\n```ts\nconst a = 1;\n```\n\n| a | b |\n| - | - |\n| 1 | 2 |\n\n![pixel](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/q842iQAAAABJRU5ErkJggg==)" },
  );
}
if (process.env.FAKE_CODEX_FILE_LINKS) {
  turns[1].items.push({ id: "file-links", type: "agentMessage", text: [
    "[Nested source](src/deep/inner.txt#L1)",
    `[Absolute readme](<${process.cwd()}/README.md:2>)`,
    `[Local file URL](${pathToFileURL(join(process.cwd(), "special # 100%.txt")).href})`,
    "[Missing file](missing.txt)",
    "[Outside file](../escape.txt)",
  ].join("\n\n") });
}
if (process.env.FAKE_CODEX_MANY) {
  const many = Array.from({ length: Number(process.env.FAKE_CODEX_MANY) }, (_, n) => ({ id: `old-${n}`, status: "completed", items: [
    { id: `ou-${n}`, type: "userMessage", content: [{ type: "text", text: `older question ${n}` }] },
    { id: `oa-${n}`, type: "agentMessage", text: `older answer ${n}` },
  ] }));
  turns.unshift(...many);
}
for await (const line of readline.createInterface({ input: process.stdin })) {
  const request = JSON.parse(line);
  if (process.env.FAKE_CODEX_CALLS) appendFileSync(process.env.FAKE_CODEX_CALLS, line + "\n");
  if (!request.id) continue;
  let result = {};
  if (request.method.startsWith("thread/goal/")) {
    if (process.env.FAKE_CODEX_NO_GOAL) { console.log(JSON.stringify({ id: request.id, error: { message: "Method not found" } })); continue; }
    if (request.method === "thread/goal/set") {
      const { origin, threadId: target, ...input } = request.params;
      if (origin !== "user") throw new Error("Goal changes require user provenance");
      goal = { objective: "", status: "active", tokenBudget: null, tokensUsed: 0, timeUsedSeconds: 0, ...goal, ...input };
      writeFileSync(goalFile, JSON.stringify(goal));
    }
    if (request.method === "thread/goal/clear") { goal = null; writeFileSync(goalFile, "null"); }
    result = request.method === "thread/goal/clear" ? { cleared: true } : { goal };
  }
  if (request.method === "model/list") result = { data: process.env.FAKE_CODEX_MODELS ? [
    { id: "gpt-a", model: "gpt-a", displayName: "GPT A", description: "", isDefault: true, hidden: false, supportedReasoningEfforts: [{ reasoningEffort: "low", description: "" }, { reasoningEffort: "high", description: "" }], defaultReasoningEffort: "low" },
    { id: "gpt-b", model: "gpt-b", displayName: "GPT B", description: "", isDefault: false, hidden: false, supportedReasoningEfforts: [{ reasoningEffort: "medium", description: "" }], defaultReasoningEffort: "medium" },
  ] : [] };
  if (request.method === "turn/start") {
    if (process.env.FAKE_CODEX_RELAY_RECOVERY) {
      assert.ok(!turns.some(turn => turn.status === "inProgress"), "recovery must not start concurrent turns");
      const turn = { id: `relay-${++relayTurn}`, status: "inProgress", items: [{ id: `relay-user-${relayTurn}`, type: "userMessage", content: request.params.input }] };
      turns.push(turn);
      result = { turn: { id: turn.id } };
      setTimeout(() => {
        turn.status = turn.id === "relay-1" ? "failed" : "completed";
        if (turn.status === "failed") turn.error = { message: process.env.FAKE_CODEX_RELAY_ERROR || "502 Bad Gateway (sealed relay fixture)" };
        else turn.items.push({ id: `answer-${turn.id}`, type: "agentMessage", text: "Relay recovered" });
        console.log(JSON.stringify({ method: "turn/completed", params: { threadId, turn } }));
      }, 40);
    } else if (process.env.FAKE_CODEX_COMPLETE_TURNS) {
      // Records the started turn and completes it for real (thread/read shows it, turn/completed is emitted), so no
      // launcher has to settle an accepted turn by guessing.
      const turn = { id: completedTurns ? `turn-new-${completedTurns}` : "turn-new", status: "completed", items: [{ id: `u-new-${completedTurns}`, type: "userMessage", content: request.params.input }, { id: `a-new-${completedTurns}`, type: "agentMessage", text: "Done." }] };
      completedTurns++; turns.push(turn); pendingCompletion = turn;
      result = { turn: { id: turn.id } };
    } else result = { turn: { id: "turn-new" } };
  }
  if (request.method === "thread/resume") {
    if (request.params.threadId === "missing") {
      console.log(JSON.stringify({ id: request.id, error: { message: "thread does not exist" } }));
      continue;
    }
    threadId = request.params.threadId;
  }
  if (["thread/start", "thread/resume", "thread/read"].includes(request.method)) {
    const failed = turns.find(turn => turn.id === "relay-1" && turn.status === "failed");
    if (request.method === "thread/read" && failed && process.env.FAKE_CODEX_RECOVERY_STATE === "not-materialized") {
      console.log(JSON.stringify({ id: request.id, error: { message: "thread not materialized" } })); continue;
    }
    if (request.method === "thread/read" && process.env.FAKE_CODEX_RECOVERY_ACTIVE && failed && !recoveryRead) {
      recoveryRead = true; failed.status = "inProgress";
      setTimeout(() => {
        failed.status = "failed";
        console.log(JSON.stringify({ method: "turn/completed", params: { threadId, turn: failed } }));
      }, 150);
    }
    // Exercise resume history even if the optional subsequent read fails.
    if (request.method === "thread/read" && process.env.FAKE_READ_FAIL) {
      console.log(JSON.stringify({ id: request.id, error: { message: "read unavailable" } }));
      continue;
    }
    const recoveryState = failed ? process.env.FAKE_CODEX_RECOVERY_STATE_FILE ? readFileSync(process.env.FAKE_CODEX_RECOVERY_STATE_FILE, "utf8") : process.env.FAKE_CODEX_RECOVERY_STATE : undefined;
    const status = recoveryState?.startsWith("systemError") ? { type: "systemError" } : recoveryState === "unknown" ? { type: "unknown" } : recoveryState === "idle" ? { type: turns.some(turn => turn.status === "inProgress") ? "active" : "idle" } : process.env.FAKE_CODEX_STATUS ? { type: process.env.FAKE_CODEX_STATUS } : undefined;
    result = { thread: { id: threadId, cwd: process.cwd(), turns: recoveryState === "missing-turns" ? undefined : recoveryState === "systemError-empty" ? [] : turns, ...(status ? { status } : {}) }, model: "test" };
  }
  if (request.method === "turn/steer" && request.params.expectedTurnId !== "turn-live") {
    console.log(JSON.stringify({ id: request.id, error: { message: `expected active turn id \`${request.params.expectedTurnId}\` but found \`turn-live\`` } }));
    continue;
  }
  console.log(JSON.stringify({ id: request.id, result }));
  if (request.method === "turn/start" && process.env.FAKE_CODEX_IMAGES) {
    const turn = { id: "turn-new", status: "completed", items: [{ id: "image-user", type: "userMessage", content: request.params.input }] };
    writeFileSync(join(process.cwd(), ".fake-image-turns.json"), JSON.stringify([turn]));
    console.log(JSON.stringify({ method: "turn/completed", params: { threadId, turn } }));
  }
  if (request.method === "thread/goal/set" || request.method === "thread/goal/clear") console.log(JSON.stringify({ method: goal ? "thread/goal/updated" : "thread/goal/cleared", params: { threadId, goal } }));
  if (pendingCompletion) { console.log(JSON.stringify({ method: "turn/completed", params: { threadId, turn: pendingCompletion } })); pendingCompletion = undefined; }
  if (request.method === "turn/start" && goal && !process.env.FAKE_CODEX_GOAL_ACTIVE) {
    goal = { ...goal, status: "complete", tokensUsed: 42, timeUsedSeconds: 2 };
    writeFileSync(goalFile, JSON.stringify(goal));
    console.log(JSON.stringify({ method: "thread/goal/updated", params: { threadId, turnId: "turn-new", goal } }));
    console.log(JSON.stringify({ method: "turn/completed", params: { threadId, turn: { id: "turn-new", status: "completed", items: [] } } }));
  }
  if (request.method === "thread/read" && process.env.FAKE_CODEX_PLAN) {
    console.log(JSON.stringify({ method: "turn/plan/updated", params: { threadId, turnId: "turn-1", explanation: "Checking the UI", plan: [{ step: "Tests", status: "completed" }, { step: "Screenshots", status: "in_progress" }] } }));
  }
}
