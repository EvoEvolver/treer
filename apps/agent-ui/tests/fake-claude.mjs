#!/usr/bin/env node
import { createInterface } from "node:readline";
import { appendFileSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
const args = process.argv.slice(2);
const arg = (name) => args[args.indexOf(name) + 1];
const sessionId = args.includes("--resume") ? arg("--resume") : arg("--session-id");
const send = (event) => console.log(JSON.stringify({ session_id: sessionId, ...event }));
const system = (subtype, rest = {}) => send({ type: "system", subtype, ...rest });
let hold = false;
let timer;
let sequence = 0;
const history = process.env.FAKE_CLAUDE_CONFIG ? join(process.env.FAKE_CLAUDE_CONFIG, "projects", process.cwd().replace(/[^a-zA-Z0-9]/g, "-"), `${sessionId}.jsonl`) : null;
const persist = (event) => {
  if (history) { mkdirSync(dirname(history), { recursive: true }); appendFileSync(history, JSON.stringify(event) + "\n"); }
};
function message(type, content, id = `m${++sequence}`, rest = {}) {
  const event = { type, uuid: `${id}-uuid`, message: { id, role: type, content }, ...rest };
  persist(event); send(event);
}
const result = (extra = {}) => { hold = false; system("session_state_changed", { state: "idle" }); send({ type: "result", result: "Done", ...extra }); };
for await (const line of createInterface({ input: process.stdin })) {
  const input = JSON.parse(line);
  if (process.env.FAKE_CLAUDE_CALLS) appendFileSync(process.env.FAKE_CLAUDE_CALLS, line + "\n");
  if (input.type === "control_request") {
    const subtype = input.request.subtype;
    const error = subtype === "initialize" && sessionId === "reject" ? "Cannot resume this session" : subtype === "set_model" && input.request.model === "invalid" ? "Invalid model" : null;
    send({ type: "control_response", response: { subtype: error ? "error" : "success", request_id: input.request_id, ...(error ? { error } : { response: subtype === "initialize" ? { models: [{ value: "sonnet", displayName: "Sonnet" }, { value: "opus", displayName: "Opus" }] } : {} }) } });
    if (subtype === "interrupt") { clearTimeout(timer); result(); }
    continue;
  }
  if (input.type !== "user") continue;
  const value = input.message.content[0].text;
  system("init", { model: "sonnet" });
  system("session_state_changed", { state: "running" });
  // Real Claude does not echo ordinary user inputs to stdout, but saves them.
  persist({ ...input, uuid: input.uuid || `u${++sequence}`, timestamp: new Date().toISOString() });
  if (hold) { message("assistant", [{ type: "text", text: `Steered: ${value}` }]); result(); continue; }
  if (value === "hold") { hold = true; continue; }
  if (value === "many-tasks") {
    for (let index = 0; index < 105; index++) system("task_notification", { task_id: `t${index}`, status: "completed", summary: "done" });
    system("task_started", { task_id: "still-running", task_type: "agent", description: "Working" });
    result(); continue;
  }
  const stream = (event) => send({ type: "stream_event", event });
  stream({ type: "message_start", message: { id: "thinking" } });
  stream({ type: "content_block_start", index: 0, content_block: { type: "thinking", thinking: "" } });
  stream({ type: "content_block_delta", index: 0, delta: { type: "thinking_delta", thinking: "Considering" } });
  message("assistant", [{ type: "thinking", thinking: "Considering" }], "thinking");
  const tools = [
    { id: "bash", name: "Bash", input: { command: "echo ok" } },
    { id: "edit", name: "Edit", input: { file_path: "note.txt", old_string: "old\n", new_string: "new\n" } },
    { id: "write", name: "Write", input: { file_path: "new.txt", content: "new file\n" } },
    { id: "multi", name: "MultiEdit", input: { file_path: "multi.txt", edits: [{ old_string: "a", new_string: "b" }, { old_string: "c", new_string: "d" }] } },
    { id: "todo", name: "TodoWrite", input: { todos: [{ content: "Inspect", status: "completed" }, { content: "Verify", status: "in_progress" }] } },
    { id: "read", name: "Read", input: { file_path: "README.md" } },
  ];
  for (const tool of tools) {
    message("assistant", [{ type: "tool_use", ...tool }]);
    message("user", [{ type: "tool_result", tool_use_id: tool.id, content: "ok", is_error: false }], undefined,
      tool.id === "edit" ? { tool_use_result: { structuredPatch: [{ oldStart: 1, oldLines: 1, newStart: 1, newLines: 1, lines: ["-old", "+new"] }] } } : {});
  }
  const output = join(process.cwd(), "task.output");
  writeFileSync(output, "x".repeat(70_000) + "TASK_DONE\n");
  system("background_tasks_changed", { tasks: [{ task_id: "task1", run_id: "run1", task_type: "local_bash", description: "Background shell" }] });
  system("task_started", { task_id: "task1", tool_use_id: "bash", is_backgrounded: true, task_type: "local_bash", description: "Background shell" });
  system("task_started", { task_id: "failed-task", task_type: "agent", description: "Failed helper" });
  system("task_notification", { task_id: "failed-task", status: "failed", summary: "failed" });
  system("task_notification", { task_id: "stopped-task", task_type: "monitor", status: "stopped", summary: "stopped" });
  timer = setTimeout(() => {
    system("task_updated", { task_id: "task1", patch: { status: "completed", end_time: Date.now(), exit_code: 0 } });
    system("task_notification", { task_id: "task1", status: "completed", output_file: output, summary: "TASK_DONE" });
    system("background_tasks_changed", { tasks: [] });
    stream({ type: "message_start", message: { id: "answer" } });
    stream({ type: "content_block_start", index: 0, content_block: { type: "text", text: "" } });
    stream({ type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "Done" } });
    message("assistant", [{ type: "text", text: "Done" }], "answer");
    result();
  }, 30);
}
