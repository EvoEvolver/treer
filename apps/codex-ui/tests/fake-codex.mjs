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
}
