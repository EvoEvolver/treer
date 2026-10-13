// Explicit opt-in, one temporary thread, bounded text-only native verification.
import { spawn } from "node:child_process";
import { mkdtemp, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { JsonRpcClient } from "../src/jsonrpc.js";
import { hermeticEnv } from "./hermetic.mjs";
if (process.env.AGENT_UI_LIVE_GOAL_EDIT !== "1") throw new Error("Set AGENT_UI_LIVE_GOAL_EDIT=1 for the explicit native probe");
const cwd = await realpath(await mkdtemp(join(tmpdir(), "codex-goal-edit-probe-")));
const child = spawn(process.env.CODEX_BIN || "codex", ["--no-daemon", "app-server", "--listen", "stdio://"], { cwd, env: hermeticEnv(), stdio: "pipe" });
child.stderr.resume();
const client = new JsonRpcClient(child.stdout, child.stdin);
let threadId = "", goalSet = false, autoTurns = 0, reply = "", completed = false;
const objective = "TREER_GOAL_EDIT_NATIVE_CONTEXT_PROBE";
client.on("request", (request) => client.respond(request.id, {}));
client.on("notification", (event) => {
  if (goalSet && event.method === "turn/started") autoTurns++;
  if (event.method === "item/agentMessage/delta") reply += String(event.params.delta || "");
  if (event.method === "turn/completed") completed = true;
});
try {
  await client.request("initialize", { clientInfo: { name: "treer-goal-edit-probe", version: "1" }, capabilities: { experimentalApi: true } }); client.notify("initialized");
  const started = await client.request<{ thread: { id: string } }>("thread/start", { cwd, approvalPolicy: "never", sandbox: "read-only" });
  threadId = started.thread.id;
  await client.request("thread/goal/set", { threadId, origin: "user", objective: "TREER_GOAL_EDIT_OLD", status: "paused", tokenBudget: 1000 });
  await client.request("thread/goal/set", { threadId, origin: "user", objective, status: "active", tokenBudget: 1000 }); goalSet = true;
  await new Promise((done) => setTimeout(done, 1000));
  const autoTurnsBeforePrompt = autoTurns;
  await client.request("turn/start", { threadId, input: [{ type: "text", text: "Reply only with the exact objective of the current goal, if known; otherwise UNKNOWN. Do not use tools.", text_elements: [] }] });
  const deadline = Date.now() + 45000;
  while (!completed && Date.now() < deadline) await new Promise((done) => setTimeout(done, 100));
  console.log(JSON.stringify({ completed, autoTurnsBeforePrompt, nextTurnSawObjective: reply.includes(objective), note: "No tools requested; native goal/set is not a launcher prompt delivery receipt" }));
  if (!completed) throw new Error("Native goal edit probe timed out");
} finally {
  if (threadId) await client.request("thread/goal/clear", { threadId, origin: "user" }).catch(() => {});
  client.close(); child.kill("SIGTERM");
}
