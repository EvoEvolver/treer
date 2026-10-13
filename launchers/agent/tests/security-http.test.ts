import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { request } from "node:http";
import test from "node:test";
import { WebSocket } from "ws";
import { hermeticEnv } from "./hermetic.mjs";
import { JSON_BODY_LIMIT } from "../src/http-input.js";

const root = resolve(import.meta.dirname, "..");
async function fixture(t: { after: (fn: () => Promise<void>) => void }) {
  const cwd = await mkdtemp(join(tmpdir(), "launcher-security-")), portFile = join(cwd, "port");
  const child = spawn(process.execPath, ["--import", "tsx", "src/index.ts"], {
    cwd: root, env: hermeticEnv({ HOME: cwd, CODEX_HOME: join(cwd, "codex"), CLAUDE_CONFIG_DIR: join(cwd, "claude"), AGENT_UI_CWD: cwd, AGENT_UI_PORT_FILE: portFile, AGENT_UI_PORT: "0", CODEX_BIN: join(root, "tests/fake-codex.mjs") }), stdio: "pipe",
  });
  let logs = "";
  child.stdout.on("data", (chunk) => { logs += chunk; }); child.stderr.on("data", (chunk) => { logs += chunk; });
  t.after(async () => { if (child.exitCode === null && child.signalCode === null) { child.kill("SIGTERM"); await once(child, "exit"); } await rm(cwd, { recursive: true, force: true }); });
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    try {
      const base = `http://127.0.0.1:${Number(await readFile(portFile, "utf8"))}`;
      if ((await fetch(base + "/api/health")).ok) return { base, logs: () => logs };
    } catch { /* isolated server startup */ }
    await new Promise((done) => setTimeout(done, 20));
  }
  throw new Error("Sealed launcher fixture failed to start");
}

test("JSON errors never echo synthetic secrets to response/logs and reject scalar bodies", async (t) => {
  const server = await fixture(t), secret = "FAKE_PRIVATE_REQUEST_CONTENT";
  for (const body of [`{\"prompt\":\"${secret}\" broken`, "null", "[]", "5"]) {
    const response = await fetch(server.base + "/api/prompt", { method: "POST", body });
    assert.equal(response.status, 400); assert.ok(!(await response.text()).includes(secret));
  }
  assert.ok(!server.logs().includes(secret));
});

test("declared and chunked oversized JSON receive 413 without submitting a turn", async (t) => {
  const server = await fixture(t);
  const before = await (await fetch(server.base + "/api/state")).json();
  const response = await fetch(server.base + "/api/prompt", { method: "POST", body: "x".repeat(JSON_BODY_LIMIT + 1) });
  assert.equal(response.status, 413);
  const chunkedStatus = await new Promise<number>((resolve, reject) => {
    const req = request(server.base + "/api/prompt", { method: "POST", headers: { "transfer-encoding": "chunked" } }, (response) => {
      response.resume(); response.on("end", () => resolve(response.statusCode!));
    });
    req.on("error", reject); req.write("x".repeat(JSON_BODY_LIMIT)); req.end("extra");
  });
  assert.equal(chunkedStatus, 413);
  const state = await (await fetch(server.base + "/api/state")).json();
  assert.equal(state.thread.turns.length, before.thread.turns.length);
});

test("oversized WebSocket client payload closes that socket without killing the launcher", async (t) => {
  const server = await fixture(t), socket = new WebSocket(server.base.replace("http", "ws") + "/ws");
  socket.on("error", () => {});
  await once(socket, "open");
  const closed = new Promise<number>((done) => socket.once("close", (code) => done(code)));
  socket.send("x".repeat(16385));
  assert.equal(await closed, 1009);
  assert.equal((await fetch(server.base + "/api/state")).status, 200);
});
