#!/usr/bin/env node
import readline from "node:readline";
import { appendFileSync } from "node:fs";

const turns = [0, 1].map((n) => ({
  id: `turn-${n}`, status: "completed", items: [
    { id: `u-${n}`, type: "userMessage", content: [{ type: "text", text: `remember ${n}` }] },
    { id: `a-${n}`, type: "agentMessage", text: `recalled ${n}` },
  ],
}));
let threadId = `new-${process.pid}`;
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
for await (const line of readline.createInterface({ input: process.stdin })) {
  const request = JSON.parse(line);
  if (process.env.FAKE_CODEX_CALLS) appendFileSync(process.env.FAKE_CODEX_CALLS, line + "\n");
  if (!request.id) continue;
  let result = {};
  if (request.method === "model/list") result = { data: [] };
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
    result = { thread: { id: threadId, cwd: process.cwd(), turns }, model: "test" };
  }
  console.log(JSON.stringify({ id: request.id, result }));
  if (request.method === "thread/read" && process.env.FAKE_CODEX_PLAN) {
    console.log(JSON.stringify({ method: "turn/plan/updated", params: { threadId, turnId: "turn-1", explanation: "Checking the UI", plan: [{ step: "Tests", status: "completed" }, { step: "Screenshots", status: "in_progress" }] } }));
  }
}
