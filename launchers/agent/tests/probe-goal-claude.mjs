// Opt-in: native goal protocol discovery in a disposable, new Claude session.
import { spawn } from "node:child_process";
import { mkdtemp, realpath, writeFile, readFile } from "node:fs/promises";
import { tmpdir, homedir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
const cwd = await realpath(await mkdtemp(join(tmpdir(), "claude-goal-probe-")));
const child = spawn(process.env.CLAUDE_BIN || "claude", ["--print", "--verbose", "--output-format", "stream-json", "--input-format", "stream-json", "--include-partial-messages", "--dangerously-skip-permissions"], { cwd, stdio: "pipe" });
const events = []; let sessionId; let started = false;
const send = (value) => child.stdin.write(JSON.stringify(value) + "\n");
// Optional discovery commands can be supplied explicitly. pause/resume are
// literal conditions on current Claude, not control commands.
const prompts = (process.env.CLAUDE_GOAL_PROBE_COMMANDS || "/goal Reply exactly GOAL_DONE and mark the goal complete. Do not use tools.|/goal|/goal clear|/goal").split("|");
createInterface({ input: child.stdout }).on("line", (line) => {
  let event; try { event = JSON.parse(line); } catch { return; }
  events.push(event); sessionId ||= event.session_id;
  if (event.type !== "control_response" && event.type !== "stream_event") console.log(JSON.stringify({ type: event.type, subtype: event.subtype, model: event.message?.model, content: event.message?.content, source: event.local_command_source, usage: event.usage, result: event.result }));
  if (event.type === "control_response" && !started) { started = true; void next(); }
});
child.stderr.on("data", (chunk) => process.stderr.write(chunk));
async function next() {
  for (const prompt of prompts) {
    console.log(JSON.stringify({ sent: prompt }));
    send({ type: "user", message: { role: "user", content: [{ type: "text", text: prompt }] } });
    await new Promise((done) => setTimeout(done, 4500));
  }
  child.stdin.end();
}
send({ type: "control_request", request_id: "init", request: { subtype: "initialize" } });
const timer = setTimeout(() => child.kill("SIGTERM"), 55000);
await new Promise((done) => child.once("exit", done)); clearTimeout(timer);
await writeFile(join(cwd, "events.json"), JSON.stringify(events, null, 2));
if (sessionId) {
  const path = join(process.env.CLAUDE_CONFIG_DIR || join(homedir(), ".claude"), "projects", cwd.replace(/[^a-zA-Z0-9]/g, "-"), `${sessionId}.jsonl`);
  const history = await readFile(path, "utf8").catch(() => "");
  await writeFile(join(cwd, "history.jsonl"), history);
  for (const line of history.trim().split("\n")) { if (!line) continue; const e = JSON.parse(line); if (/goal/i.test(e.type) || e.type === "system") console.log(JSON.stringify({ history: e })); }
}
console.log(JSON.stringify({ cwd, sessionId }));
