// Native protocol discovery in a new disposable session only.
import { spawn } from "node:child_process";
import { mkdtemp, realpath, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
const cwd = await realpath(await mkdtemp(join(tmpdir(), "claude-settings-probe-")));
const child = spawn(process.env.CLAUDE_BIN || "claude", ["--print", "--verbose", "--input-format", "stream-json", "--output-format", "stream-json", "--include-partial-messages", "--dangerously-skip-permissions", "--model", "sonnet", "--effort", "low"], { cwd, stdio: "pipe" });
const events = []; const pending = new Map(); let nextId = 0; let switched = false; let results = 0;
const send = (event) => child.stdin.write(JSON.stringify(event) + "\n");
function control(request) {
  const request_id = String(++nextId);
  return new Promise((resolve) => { pending.set(request_id, resolve); send({ type: "control_request", request_id, request }); });
}
const prompt = (text) => send({ type: "user", message: { role: "user", content: [{ type: "text", text }] } });
createInterface({ input: child.stdout }).on("line", (line) => {
  let e; try { e = JSON.parse(line); } catch { return; } events.push(e);
  if (e.type === "control_response") { pending.get(e.response.request_id)?.(e.response); pending.delete(e.response.request_id); }
  if (e.type === "system" && e.subtype === "init") console.log(JSON.stringify({ init: { keys: Object.keys(e), model: e.model, effort: e.effort, thinking: e.thinking } }));
  if (e.type === "assistant") {
    console.log(JSON.stringify({ assistant: { model: e.message?.model, content: e.message?.content?.filter((b) => b.type !== "thinking") } }));
    if (!switched && e.message?.content?.some((b) => b.type === "tool_use" && b.name === "Bash")) {
      switched = true;
      void (async () => {
        console.log(JSON.stringify({ setModel: await control({ subtype: "set_model", model: "haiku" }) }));
        console.log(JSON.stringify({ setEffort: await control({ subtype: "set_effort", effort: "high" }) }));
        console.log(JSON.stringify({ applyFlags: await control({ subtype: "apply_flag_settings", settings: { effortLevel: "high" } }) }));
      })();
    }
  }
  if (e.type === "result") {
    console.log(JSON.stringify({ result: e.result, usage: e.modelUsage }));
    results++;
    if (results === 1) prompt("Reply exactly SECOND_DONE. No tools.");
    if (results === 2) prompt("/model");
    if (results === 3) prompt("/effort medium");
    if (results === 4) prompt("/model");
    if (results === 5) child.stdin.end();
  }
});
child.stderr.on("data", (chunk) => process.stderr.write(chunk));
const initialized = await control({ subtype: "initialize" });
console.log(JSON.stringify({ initialize: { models: initialized.response?.models, commands: initialized.response?.commands?.filter((c) => ["model", "effort"].includes(c.name)), keys: Object.keys(initialized.response || {}) } }));
prompt("Protocol test in a disposable directory. Call Bash foreground exactly once: sleep 4; echo FIRST_DONE. Then reply FIRST_DONE. Do not read files or do anything else.");
const timer = setTimeout(() => child.kill("SIGTERM"), 70000);
await new Promise((done) => child.once("exit", done)); clearTimeout(timer);
await writeFile(join(cwd, "events.json"), JSON.stringify(events, null, 2));
console.log(JSON.stringify({ cwd, results, switched }));
