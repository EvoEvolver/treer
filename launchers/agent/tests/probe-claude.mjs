// Opt-in protocol experiment: new Claude sessions in a temporary directory only.
import { spawn } from "node:child_process";
import { mkdtemp, realpath, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
const cwd = await realpath(await mkdtemp(join(tmpdir(), "agent-ui-claude-probe-")));
const child = spawn(process.env.CLAUDE_BIN || "claude", ["--print", "--verbose", "--output-format", "stream-json", "--input-format", "stream-json", "--include-partial-messages", "--dangerously-skip-permissions"], { cwd, stdio: "pipe" });
const started = Date.now();
const events = [];
const send = (value) => child.stdin.write(JSON.stringify(value) + "\n");
const user = (text, extra = {}) => send({ type: "user", message: { role: "user", content: [{ type: "text", text }] }, ...extra });
let injected = false;
let prompted = false;
let results = 0;
const prompt = () => { if (prompted) return; prompted = true; user(process.env.CLAUDE_PROBE_BACKGROUND ? "Protocol test in a disposable directory. Call Bash exactly once with command: sleep 3; printf 'TASK_DONE\\n', run_in_background true, description 'Background protocol test'. Then wait for the task notification and reply TASK_DONE. Do not read other files." : "Protocol test in a disposable directory. First call Bash to run exactly: sleep 6; printf 'BOUNDARY_DONE\\n'. Keep it foreground (run_in_background false). After it finishes reply FIRST_DONE. Do not read any other files."); };
child.stderr.on("data", (chunk) => process.stderr.write(chunk));
createInterface({ input: child.stdout }).on("line", (line) => {
  let event; try { event = JSON.parse(line); } catch { return; }
  events.push(event);
  const summary = { ms: Date.now() - started, type: event.type, subtype: event.subtype, controlSubtype: event.response?.subtype, requestId: event.response?.request_id, session_id: event.session_id, eventType: event.event?.type, content: event.message?.content, result: event.result };
  if (event.type !== "stream_event") console.log(JSON.stringify(summary));
  if (event.type === "control_response") prompt();
  if (!process.env.CLAUDE_PROBE_BACKGROUND && event.type === "assistant" && event.message?.content?.some((part) => part.type === "tool_use" && part.name === "Bash") && !injected) {
    injected = true;
    setTimeout(() => { const priority = process.env.CLAUDE_PROBE_PRIORITY; console.log(JSON.stringify({ ms: Date.now() - started, injected: true, priority: priority ?? "omitted" })); user("This is a mid-tool user message. After the current tool finishes, include SECOND_SEEN in your final response as well as FIRST_DONE. Do not start another Bash command.", priority ? { priority } : {}); }, 500);
  }
  if (event.type === "result" && ++results >= (process.env.CLAUDE_PROBE_PRIORITY ? 2 : 1)) child.stdin.end();
});
send({ type: "control_request", request_id: "probe-init", request: { subtype: "initialize" } });
setTimeout(prompt, 5000).unref();
const timer = setTimeout(() => child.kill("SIGTERM"), 55000);
await new Promise((done) => child.once("exit", done));
clearTimeout(timer);
await writeFile(join(cwd, "protocol.json"), JSON.stringify(events, null, 2), { mode: 0o600 });
console.log(JSON.stringify({ cwd, results, injected, exitCode: child.exitCode }));
