#!/usr/bin/env node
import readline from "node:readline";
import { appendFileSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
const goalFile = join(process.cwd(), ".fake-codex-goal.json");
let goal = null;
try { goal = JSON.parse(readFileSync(goalFile, "utf8")); } catch { /* no goal */ }

const turns = [0, 1].map((n) => ({
  id: `turn-${n}`, status: "completed", items: [
    { id: `u-${n}`, type: "userMessage", content: [{ type: "text", text: `remember ${n}` }] },
    { id: `a-${n}`, type: "agentMessage", text: `recalled ${n}` },
  ],
}));
let threadId = `new-${process.pid}`;
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
    { id: "rich-1", type: "agentMessage", text: "## Result\n\nSee [the docs](https://example.test/docs) and [bad](javascript:window.xss=3).\n\n<img src=x onerror=\"window.xss=1\">\n\n<svg onload=\"window.xss=2\"></svg><script>window.xss=4</script>\n\n![remote](https://example.test/x.png)\n\n- one\n- two\n\n```ts\nconst a = 1;\n```\n\n| a | b |\n| - | - |\n| 1 | 2 |\n\n![pixel](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/q842iQAAAABJRU5ErkJggg==)" },
  );
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
  if (request.method === "turn/start") result = { turn: { id: "turn-new" } };
  if (request.method === "thread/resume") {
    if (request.params.threadId === "missing") {
      console.log(JSON.stringify({ id: request.id, error: { message: "thread does not exist" } }));
      continue;
    }
    threadId = request.params.threadId;
  }
  if (["thread/start", "thread/resume", "thread/read"].includes(request.method)) {
    // Exercise resume history even if the optional subsequent read fails.
    if (request.method === "thread/read" && process.env.FAKE_READ_FAIL) {
      console.log(JSON.stringify({ id: request.id, error: { message: "read unavailable" } }));
      continue;
    }
    const status = process.env.FAKE_CODEX_STATUS ? { type: process.env.FAKE_CODEX_STATUS } : undefined;
    result = { thread: { id: threadId, cwd: process.cwd(), turns, ...(status ? { status } : {}) }, model: "test" };
  }
  if (request.method === "turn/steer" && request.params.expectedTurnId !== "turn-live") {
    console.log(JSON.stringify({ id: request.id, error: { message: `expected active turn id \`${request.params.expectedTurnId}\` but found \`turn-live\`` } }));
    continue;
  }
  console.log(JSON.stringify({ id: request.id, result }));
  if (request.method === "thread/goal/set" || request.method === "thread/goal/clear") console.log(JSON.stringify({ method: goal ? "thread/goal/updated" : "thread/goal/cleared", params: { threadId, goal } }));
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
