#!/usr/bin/env node
// Minimal ACP agent for adapter tests. Records every request in FAKE_ACP_CALLS
// (JSONL). FAKE_ACP_NO_LOAD=1 hides loadSession; FAKE_ACP_HANG=1 keeps a prompt
// running until session/cancel.
import { appendFileSync, writeFileSync } from "node:fs";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";

const calls = process.env.FAKE_ACP_CALLS;
// Opt-in cleanup probe for native smoke; normal adapter fixtures stay unchanged.
if (process.env.FAKE_ACP_CHILD_PID_FILE) {
  const child = spawn(process.execPath, ["-e", "setInterval(()=>{},1000)"], { stdio: "ignore", windowsHide: true });
  writeFileSync(process.env.FAKE_ACP_CHILD_PID_FILE, JSON.stringify([process.pid, child.pid]));
}
let nextId = 1000;
const waiting = new Map();
let pendingPrompt = null;
let mode = "default";
let model = "sonnet";

const write = (message) => process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", ...message })}\n`);
const update = (sessionId, value) => write({ method: "session/update", params: { sessionId, update: value } });
const ask = (method, params) => new Promise((resolve) => { const id = nextId++; waiting.set(id, resolve); write({ id, method, params }); });

function configOptions() {
  return [
    { id: "mode", category: "mode", type: "select", currentValue: mode, options: [
      { value: "default", name: "Manual" }, { value: "bypassPermissions", name: "Bypass permissions" }] },
    { id: "model", category: "model", type: "select", currentValue: model, options: [
      { value: "sonnet", name: "Sonnet", description: "Balanced" }, { value: "opus", name: "Opus", description: "Deep" }] },
  ];
}

function replay(sessionId) {
  update(sessionId, { sessionUpdate: "user_message_chunk", messageId: "u1", content: { type: "text", text: "Remember " } });
  update(sessionId, { sessionUpdate: "user_message_chunk", messageId: "u1", content: { type: "text", text: "PLUM-42" } });
  update(sessionId, { sessionUpdate: "agent_message_chunk", messageId: "a1", content: { type: "text", text: "noted" } });
  update(sessionId, { sessionUpdate: "user_message_chunk", messageId: "u2", content: { type: "text", text: "list files" } });
  update(sessionId, { sessionUpdate: "tool_call", toolCallId: "t-old", title: "ls", kind: "execute", status: "completed",
    rawInput: { command: "ls" }, content: [{ type: "content", content: { type: "text", text: "a.txt" } }] });
}

async function handle(message) {
  if (calls) appendFileSync(calls, `${JSON.stringify({ method: message.method, params: message.params })}\n`);
  const { id, method, params } = message;
  switch (method) {
    case "initialize":
      return write({ id, result: { protocolVersion: 1, agentInfo: { name: "fake-acp", title: "Fake Agent" },
        agentCapabilities: { loadSession: process.env.FAKE_ACP_NO_LOAD !== "1" } } });
    case "session/new":
      return write({ id, result: { sessionId: "sess-new", configOptions: configOptions() } });
    case "session/load":
      if (params.sessionId === "missing") return write({ id, error: { code: -32002, message: "session not found" } });
      replay(params.sessionId);
      return write({ id, result: { configOptions: configOptions() } });
    case "session/set_config_option":
      if (params.configId === "mode") mode = params.value;
      if (params.configId === "model") model = params.value;
      return write({ id, result: { configOptions: configOptions() } });
    case "session/prompt": {
      const sessionId = params.sessionId;
      if (process.env.FAKE_ACP_HANG === "1") { pendingPrompt = { id }; return; }
      update(sessionId, { sessionUpdate: "agent_thought_chunk", messageId: "m1", content: { type: "text", text: "Thinking" } });
      update(sessionId, { sessionUpdate: "plan", entries: [
        { content: "Inspect", status: "completed", priority: "high" }, { content: "Edit", status: "in_progress", priority: "high" }] });
      update(sessionId, { sessionUpdate: "tool_call", toolCallId: "t1", title: "Run tests", kind: "execute", status: "pending", rawInput: {} });
      update(sessionId, { sessionUpdate: "tool_call_update", toolCallId: "t1", rawInput: { command: "npm test" } });
      update(sessionId, { sessionUpdate: "tool_call_update", toolCallId: "t1", status: "completed",
        content: [{ type: "content", content: { type: "text", text: "13 passed" } }] });
      const permission = await ask("session/request_permission", { sessionId, toolCall: { toolCallId: "t2" }, options: [
        { optionId: "reject", name: "Reject", kind: "reject_once" }, { optionId: "always", name: "Always", kind: "allow_always" }] });
      update(sessionId, { sessionUpdate: "tool_call", toolCallId: "t2", title: "Edit note.txt", kind: "edit", status: "completed",
        content: [{ type: "diff", path: "note.txt", oldText: "a\nb\n", newText: "a\nc\n" }], rawOutput: permission });
      update(sessionId, { sessionUpdate: "agent_message_chunk", messageId: "m2", content: { type: "text", text: "Done " } });
      update(sessionId, { sessionUpdate: "agent_message_chunk", messageId: "m2", content: { type: "text", text: "now." } });
      return write({ id, result: { stopReason: "end_turn" } });
    }
    case "session/cancel":
      if (pendingPrompt) { write({ id: pendingPrompt.id, result: { stopReason: "cancelled" } }); pendingPrompt = null; }
      return;
    default:
      if (id !== undefined) write({ id, error: { code: -32601, message: `unknown ${method}` } });
  }
}

createInterface({ input: process.stdin }).on("line", (line) => {
  if (!line.trim()) return;
  const message = JSON.parse(line);
  if (message.method === undefined && waiting.has(message.id)) {
    if (calls) appendFileSync(calls, `${JSON.stringify({ response: message.result ?? message.error })}\n`);
    waiting.get(message.id)(message.result);
    waiting.delete(message.id);
    return;
  }
  void handle(message);
});
