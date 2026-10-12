// Explicit opt-in integration check. Uses real Codex inference and creates
// only a temporary profile and two temporary Agents, all removed in finally.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { resolve, relative, join } from "node:path";
import { randomUUID } from "node:crypto";

function treer(...args) {
  return JSON.parse(execFileSync("treer", args, { encoding: "utf8", timeout: 30000 }));
}
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(description, callback) {
  const deadline = Date.now() + 120000;
  while (Date.now() < deadline) {
    const result = await callback();
    if (result) return result;
    await delay(500);
  }
  throw new Error(`Timeout: ${description}`);
}
if (!process.env.TREER_AGENT_ID || !process.env.TREER_WORKLOAD_CREDENTIAL || !process.env.TREER_AGENT_SERVER_URL) {
  throw new Error("Run this check inside a managed Treer Agent");
}
const context = treer("whoami");
const app = resolve(import.meta.dirname, "..");
const cwd = resolve(app, "../..");
const machineCwd = relative(context.machine.root, cwd);
assert.ok(!machineCwd.startsWith(".."));
const launcher = join(app, "scripts/treer-agent.sh");
const suffix = randomUUID().slice(0, 8);
const profile = `codex-ui-resume-test-${suffix}`;
const agents = [];
let createdProfile = false;
async function interfaceReady(id, generation = 0) {
  return until(`interface for ${id}`, async () => {
    const agent = treer("agent", "show", id);
    if (agent.status === "failed" || agent.status === "exited") {
      throw new Error(JSON.stringify(treer("agent", "read", id, "--lines", "60")));
    }
    if (agent.run_generation <= generation || !agent.interface?.port) return null;
    const base = `http://127.0.0.1:${agent.interface.port}`;
    const health = await fetch(base + "/v1/health").then((r) => r.ok).catch(() => false);
    if (!health) return null;
    const state = await (await fetch(base + "/api/state")).json();
    return { agent, base, state };
  });
}
try {
  // The exact Codex + UI recipe command, without changing an operator profile.
  treer("agent", "admin", "profile", "create", profile, "--cwd", machineCwd, launcher);
  createdProfile = true;
  for (let n = 0; n < 2; n++) {
    const result = treer("agent", "admin", "profile", "launch", profile, "--machine", context.machine.server_id, "--name", `${profile}-${n}`);
    agents.push(result.agent_id ?? result.agent?.agent_id);
    assert.ok(agents.at(-1));
  }
  const first = await interfaceReady(agents[0]);
  const second = await interfaceReady(agents[1]);
  assert.notEqual(first.agent.interface.port, second.agent.interface.port);
  assert.equal(first.agent.interface.ui_path, "/");
  assert.equal(second.agent.interface.ui_path, "/");
  const id = first.state.thread.id;
  const code = `treer-ui-context-${suffix}`;
  treer("agent", "prompt", agents[0], `Remember the validation code ${code}. Reply with just STORED. Do not use tools.`);
  const completed = await until("initial turn completed", async () => {
    const state = await (await fetch(first.base + "/api/state")).json();
    const items = state.thread.turns.flatMap((t) => t.items);
    if (state.thread.status === "error") throw new Error(JSON.stringify(state.thread.lastError));
    return state.thread.status === "idle" && items.some((i) => i.kind === "agentMessage" && i.text.includes("STORED")) ? state : null;
  });
  assert.ok(completed.thread.turns.some((t) => t.items.some((i) => i.text.includes(code))));
  const saved = JSON.parse(readFileSync(join(cwd, ".treer/agents", agents[0], "codex-ui.json"), "utf8"));
  assert.equal(saved.threadId, id);
  treer("agent", "restart", agents[0]);
  const restored = await interfaceReady(agents[0], first.agent.run_generation);
  assert.equal(restored.state.thread.id, id);
  assert.ok(restored.state.thread.turns.some((t) => t.items.some((i) => i.text.includes(code))));
  const transcript = treer("agent", "transcript", agents[0], "--page", "0");
  assert.ok(JSON.stringify(transcript).includes(code));
  treer("agent", "prompt", agents[0], "What was the validation code I asked you to remember? Reply with just that code. Do not use tools.");
  await until("restored Codex recalls context", async () => {
    const state = await (await fetch(restored.base + "/api/state")).json();
    if (state.thread.status === "error") throw new Error(JSON.stringify(state.thread.lastError));
    return state.thread.status === "idle" && state.thread.turns.flatMap((t) => t.items).some((i) => i.kind === "agentMessage" && i.text.includes(code));
  });
  console.log(JSON.stringify({ ok: true, thread_id: id, restart_generation: restored.agent.run_generation,
    ports: [restored.agent.interface.port, second.agent.interface.port], history_restored: true, context_recalled: true }, null, 2));
} finally {
  for (const id of agents) if (id) treer("agent", "admin", "delete", id);
  if (createdProfile) treer("agent", "admin", "profile", "delete", profile);
  console.log("Temporary Agents and profile removed");
}
