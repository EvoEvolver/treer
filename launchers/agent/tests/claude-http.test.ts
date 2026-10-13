import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { test } from "node:test";
import { WebSocket } from "ws";
import { hermeticEnv, localFetch as fetch } from "./hermetic.mjs";
const root = resolve(import.meta.dirname, "..");
const fake = join(root, "tests/fake-claude.mjs");
const temp = () => realpathSync(mkdtempSync(join(tmpdir(), "agent-ui-claude-http-")));

test("native Claude HTTP/WS state, queue deletion, AIS compatibility and bounded task output", async () => {
  const cwd = temp(); const portFile = join(cwd, "port");
  const child = spawn(join(root, "node_modules/.bin/tsx"), [join(root, "src/index.ts")], {
    env: hermeticEnv({ AGENT_UI_HARNESS: "claude", AGENT_UI_CWD: cwd, AGENT_UI_PORT: "0", AGENT_UI_PORT_FILE: portFile, CLAUDE_BIN: fake }), stdio: "pipe",
  });
  let logs = ""; child.stderr.on("data", (chunk) => { logs += chunk; });
  let socket: WebSocket | undefined;
  try {
    let base = "";
    for (let index = 0; index < 100; index++) {
      if (child.exitCode !== null) throw new Error(logs);
      try { base = `http://127.0.0.1:${Number(readFileSync(portFile, "utf8"))}`; if ((await fetch(base + "/api/health")).ok) break; } catch { /* startup */ }
      await new Promise((done) => setTimeout(done, 25));
    }
    const state = () => fetch(base + "/api/state").then((response) => response.json());
    const post = (path: string, body: unknown) => fetch(base + path, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    socket = new WebSocket(base.replace("http:", "ws:") + "/ws");
    const initial = await new Promise<Record<string, any>>((done, reject) => { socket!.once("message", (data) => done(JSON.parse(String(data)))); socket!.once("error", reject); });
    assert.equal(initial.type, "state"); assert.equal(initial.harness.id, "claude");
    assert.deepEqual(initial.backgroundTasks, []); assert.deepEqual(initial.queue, []);
    assert.equal((await post("/v1/prompts", { operation_id: "hold", text: "hold", mode: "prompt" })).status, 202);
    assert.equal((await post("/v1/prompts", { operation_id: "hold", text: "hold", mode: "prompt" }).then((response) => response.json())).duplicate, true);
    const queued = await post("/api/prompt", { prompt: "remove me", mode: "queue" }).then((response) => response.json());
    assert.equal(queued.submission.disposition, "queued"); assert.equal(queued.queue[0].text, "remove me");
    assert.equal((await fetch(base + "/api/queue/" + queued.submission.id, { method: "DELETE" })).status, 200);
    assert.equal((await fetch(base + "/api/queue/" + queued.submission.id, { method: "DELETE" })).status, 404);
    assert.equal((await post("/api/prompt", { prompt: "bad", mode: "priority" })).status, 400);
    const aisQueue = await post("/v1/prompts", { operation_id: "after", text: "work", mode: "prompt", delivery: "queue" }).then((response) => response.json());
    assert.equal(aisQueue.submission.disposition, "queued");
    const steer = await post("/api/prompt", { prompt: "steer now", mode: "steer" }).then((response) => response.json());
    assert.equal(steer.submission.disposition, "steered");
    for (let index = 0; index < 100; index++) {
      const snapshot = await state();
      if (snapshot.thread.status === "idle" && snapshot.backgroundTasks.some((task: any) => task.id === "task1" && task.status === "completed")) break;
      await new Promise((done) => setTimeout(done, 20));
    }
    const snapshot = await state();
    assert.equal(snapshot.thread.status, "idle"); assert.equal(snapshot.queue.length, 0);
    const output = await fetch(base + "/api/tasks/task1/output?tail=100000").then((response) => response.json());
    assert.equal(Buffer.byteLength(output.content), 65536); assert.equal(output.truncated, true);
    assert.equal((await fetch(base + "/api/tasks/task1/output?tail=10").then((response) => response.json())).content, "TASK_DONE\n");
    assert.equal((await fetch(base + "/api/tasks/missing/output")).status, 404);
    assert.equal((await fetch(base + "/api/tasks/task1/output?tail=-1")).status, 400);
    assert.equal((await fetch(base + "/v1/transcript?limit=100").then((response) => response.json())).page_count, 2);
  } finally {
    socket?.terminate();
    const exited = child.exitCode === null ? new Promise((done) => child.once("exit", done)) : Promise.resolve();
    child.kill("SIGTERM"); await exited;
  }
});

test("Claude switch dry-run uses native harness and CLAUDE_BIN without restarting", () => {
  const cwd = temp(); const log = join(cwd, "calls");
  writeFileSync(join(cwd, "treer"), `#!/bin/sh\nprintf '%s\\n' "$*" >> '${log}'\nif [ "$1" = whoami ]; then printf '%s\\n' '{"agent":{"agent_id":"ag_test"}}'; else exit 99; fi\n`, { mode: 0o700 });
  const result = execFileSync(join(root, "scripts/switch.sh"), ["--harness", "claude", "--session-id", "native-session", "--dry-run"], {
    cwd, encoding: "utf8", env: hermeticEnv({ PATH: `${cwd}:${process.env.PATH}`, CLAUDE_BIN: fake, TREER_AGENT_ID: "ag_test", TREER_WORKLOAD_CREDENTIAL: "test", TREER_AGENT_SERVER_URL: "test" }),
  });
  const plan = JSON.parse(result);
  assert.equal(plan.harness, "claude"); assert.equal(plan.session_id, "native-session");
  assert.ok(plan.startup_command.includes("claude")); assert.ok(!plan.startup_command.includes("acp"));
  assert.equal(readFileSync(log, "utf8"), "whoami\n");
});
