#!/usr/bin/env node
import { createInterface } from "node:readline";
import { appendFileSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { randomUUID } from "node:crypto";
const args = process.argv.slice(2);
const arg = (name) => args[args.indexOf(name) + 1];
const sessionId = args.includes("--resume") ? arg("--resume") : arg("--session-id");
const send = (event) => console.log(JSON.stringify({ session_id: sessionId, ...event }));
const system = (subtype, rest = {}) => send({ type: "system", subtype, ...rest });
let hold = false;
let timer;
let goalTimer;
let sequence = 0;
const resolved = process.env.FAKE_CLAUDE_RESOLVED_MODELS;
const aliases = { default: "claude-sonnet-test", sonnet: "claude-sonnet-test", opus: "claude-opus-test", legacy: "claude-legacy-test" };
let currentModel = args.includes("--model") ? arg("--model") : "sonnet";
const resolveModel = (model) => resolved ? aliases[model] || model : model;
currentModel = resolveModel(currentModel);
const history = process.env.FAKE_CLAUDE_CONFIG ? join(process.env.FAKE_CLAUDE_CONFIG, "projects", process.cwd().replace(/[^a-zA-Z0-9]/g, "-"), `${sessionId}.jsonl`) : null;
const persist = (event) => {
  if (history) { mkdirSync(dirname(history), { recursive: true }); appendFileSync(history, JSON.stringify(event) + "\n"); }
};
function message(type, content, id = `m${++sequence}`, rest = {}) {
  const event = { type, uuid: `${id}-uuid`, message: { id, role: type, content, ...(type === "assistant" ? { model: currentModel } : {}) }, ...rest };
  persist(event); send(event);
}
const result = (extra = {}) => { hold = false; system("session_state_changed", { state: "idle" }); send({ type: "result", result: "Done", ...extra }); };
for await (const line of createInterface({ input: process.stdin })) {
  const input = JSON.parse(line);
  if (process.env.FAKE_CLAUDE_CALLS) appendFileSync(process.env.FAKE_CLAUDE_CALLS, line + "\n");
  if (input.type === "control_request") {
    const subtype = input.request.subtype;
    const error = subtype === "initialize" && sessionId === "reject" ? "Cannot resume this session" : subtype === "set_model" && input.request.model === "invalid" ? "Invalid model" : subtype === "apply_flag_settings" && process.env.FAKE_CLAUDE_NO_EFFORT ? "Unsupported control request subtype: apply_flag_settings" : null;
    const models = resolved ? Object.entries(aliases).map(([value, resolvedModel]) => ({ value, resolvedModel, displayName: value === "default" ? "Default" : `${value} test`, supportsEffort: value !== "legacy", supportedEffortLevels: ["low", "medium", "high", "future-unknown"] })) : [{ value: "sonnet", displayName: "Sonnet" }, { value: "opus", displayName: "Opus" }];
    const respond = () => {
      if (!error && subtype === "set_model") currentModel = resolveModel(input.request.model);
      send({ type: "control_response", response: { subtype: error ? "error" : "success", request_id: input.request_id, ...(error ? { error } : { response: subtype === "initialize" ? { commands: process.env.FAKE_CLAUDE_NO_GOAL ? [] : [{ name: "goal" }], ...(process.env.FAKE_CLAUDE_NO_MODELS ? {} : { models }) } : {} }) } });
    };
    if (subtype === "set_model" && process.env.FAKE_CLAUDE_SLOW_SETTINGS) setTimeout(respond, 50);
    else respond();
    if (subtype === "interrupt") { clearTimeout(timer); result(); }
    continue;
  }
  if (input.type !== "user") continue;
  const value = input.message.content[0].text;
  system("init", { model: currentModel });
  system("session_state_changed", { state: "running" });
  // Real Claude does not echo ordinary user inputs to stdout, but saves them.
  persist({ ...input, uuid: input.uuid || `u${++sequence}`, timestamp: new Date().toISOString() });
  const goalCommand = /^\/goal(?:\s+([\s\S]*))?$/.exec(value);
  if (goalCommand) {
    const args = goalCommand[1] || "";
    const output = args === "clear" ? process.env.FAKE_CLAUDE_OLD_GOAL ? "Goal cleared: old condition" : "Goal cleared" : args ? `Goal set: ${args}` : "No goal set. Usage: `/goal <condition>`";
    const uuid = `goal-${randomUUID()}`;
    persist({ type: "system", subtype: "local_command", uuid, content: `<local-command-stdout>${output}</local-command-stdout>`, commandRun: { command: "goal", args } });
    send({ type: "assistant", uuid, message: { model: "<synthetic>", id: uuid, content: [{ type: "text", text: output }] }, local_command_source: `<local-command-stdout>${output}</local-command-stdout>`, local_command_run: { command: "goal", args } });
    if (args === "clear") clearInterval(goalTimer);
    if (args.startsWith("reject-loop") || args.startsWith("progress-loop") || args.startsWith("token-loop") || args.startsWith("impossible-loop")) {
      let count = 0;
      goalTimer = setInterval(() => {
        count++;
        if (args.startsWith("progress-loop")) message("assistant", [{ type: "tool_use", id: `progress-${count}`, name: "Read", input: { file_path: `evidence-${count}` } }]);
        if (args.startsWith("token-loop")) message("assistant", [{ type: "text", text: "still working" }], `usage-${count}`, { message: { id: `usage-${count}`, role: "assistant", model: currentModel, content: [{ type: "text", text: "still working" }], usage: { input_tokens: 100, output_tokens: 100 } } });
        const event = { type: "system", subtype: "stop_hook_summary", uuid: `reject-${randomUUID()}`, hookInfos: [{ promptText: args, ...(args.startsWith("impossible-loop") ? { result: { ok: false, impossible: true, reason: "Owner must create environment" } } : {}) }], hookErrors: [], hasOutput: true, preventedContinuation: true };
        persist(event); send(event);
      }, 30);
      continue;
    }
    if (args && args !== "clear") {
      message("assistant", [{ type: "text", text: "GOAL_DONE" }], undefined, {});
      persist({ type: "system", subtype: "stop_hook_summary", uuid: `hook-${randomUUID()}`, hookInfos: [{ promptText: args }], hookErrors: [], hasOutput: args.includes("hold"), preventedContinuation: false });
    }
    result({ result: args && args !== "clear" ? "GOAL_DONE" : output, usage: { input_tokens: 2, output_tokens: 3, cache_read_input_tokens: 5, cache_creation_input_tokens: 7 }, duration_ms: 1500 });
    continue;
  }
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
  system("task_started", { task_id: "foreground", is_backgrounded: false, task_type: "local_bash", description: "Foreground shell" });
  system("task_notification", { task_id: "foreground", status: "completed", output_file: "", summary: "Foreground shell" });
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
