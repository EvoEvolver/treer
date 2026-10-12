import { spawn } from "node:child_process";
import assert from "node:assert/strict";
import { mkdtemp, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { JsonRpcClient } from "../src/jsonrpc.js";
const cwd = await realpath(await mkdtemp(join(tmpdir(), "codex-goal-probe-")));
const child = spawn(process.env.CODEX_BIN || "codex", ["app-server", "--listen", "stdio://"], { cwd, stdio: "pipe" });
const client = new JsonRpcClient(child.stdout, child.stdin);
let complete = false;
client.on("notification", (e) => { if (e.method.includes("goal") || e.method === "turn/completed") console.log(JSON.stringify(e)); if (e.method === "thread/goal/updated" && e.params.goal.status === "complete") complete = true; });
client.on("request", (e) => client.respond(e.id, {}));
try {
  await client.request("initialize", { clientInfo: { name: "treer-goal-probe", version: "1" }, capabilities: { experimentalApi: true } }); client.notify("initialized");
  const { thread } = await client.request<{ thread: { id: string } }>("thread/start", { cwd, approvalPolicy: "never", sandbox: "danger-full-access", model: "gpt-6.1-sol" });
  const threadId = thread.id;
  console.log(JSON.stringify({ cwd, threadId }));
  console.log(await client.request("thread/goal/set", { threadId, origin: "user", objective: "Reply exactly GOAL_DONE and mark this goal complete. Do not use other tools.", tokenBudget: 20000 }));
  console.log(await client.request("thread/goal/get", { threadId }));
  await client.request("turn/start", { threadId, input: [{ type: "text", text: "Complete the current goal now.", text_elements: [] }] });
  const deadline = Date.now() + 45000;
  while (!complete && Date.now() < deadline) await new Promise((done) => setTimeout(done, 100));
  console.log(await client.request("thread/goal/clear", { threadId, origin: "user" }));
  console.log(JSON.stringify({ complete, goal: await client.request("thread/goal/get", { threadId }) }));
  assert.ok(complete, "Native Codex did not report goal completion");
} finally { client.close(); child.kill("SIGTERM"); }
