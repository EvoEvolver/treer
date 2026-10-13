import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { request } from "node:http";
import test from "node:test";
import { WebSocket } from "ws";
import { hermeticEnv, localFetch as fetch } from "./hermetic.mjs";
import { JSON_BODY_LIMIT } from "../src/http-input.js";

const root = resolve(import.meta.dirname, "..");
async function fixture(t: { after: (fn: () => Promise<void>) => void }) {
  const cwd = await mkdtemp(join(tmpdir(), "launcher-security-")), portFile = join(cwd, "port");
  const child = spawn(process.execPath, ["--import", "tsx", "src/index.ts"], {
    cwd: root, env: hermeticEnv({ TREER_AGENT_ID: "ag_security", AGENT_UI_INSTANCE_ID: "security-instance", HOME: cwd, CODEX_HOME: join(cwd, "codex"), CLAUDE_CONFIG_DIR: join(cwd, "claude"), AGENT_UI_CWD: cwd, AGENT_UI_PORT_FILE: portFile, AGENT_UI_PORT: "0", CODEX_BIN: join(root, "tests/fake-codex.mjs") }), stdio: "pipe",
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

test("malformed absolute request URL is rejected without unhandled rejection or process exit", async (t) => {
  const server = await fixture(t), url = new URL(server.base);
  const status = await new Promise<number>((resolve, reject) => {
    const req = request({ hostname: url.hostname, port: url.port, path: "http://%", method: "GET" }, (response) => {
      response.resume(); response.on("end", () => resolve(response.statusCode!));
    });
    req.on("error", reject); req.end();
  });
  assert.equal(status, 400);
  assert.equal((await fetch(server.base + "/api/state")).status, 200);
});

test("every browser HTTP surface rejects unmarked Origin/Fetch Metadata and foreign Host", async (t) => {
  const server = await fixture(t);
  const routes = [["/", "GET"], ["/api/state", "GET"], ["/v1/transcript", "GET"], ["/v1/files/tree", "GET"], ["/v1/files?path=.env", "GET"], ["/api/prompt", "POST"], ["/api/interrupt", "POST"], ["/api/settings", "POST"], ["/api/goal", "PUT"], ["/v1/abort", "POST"], ["/api/prompt", "OPTIONS"]];
  for (const [path, method] of routes) {
    for (const headers of [{ origin: "https://attacker.invalid" }, { origin: "null" }, { "sec-fetch-site": "same-origin" }, { "sec-fetch-mode": "navigate" }, { host: "rebinding.invalid" }]) {
      const response = await fetch(server.base + path, { method, headers });
      assert.equal(response.status, 403, `${method} ${path} ${JSON.stringify(headers)}`);
      assert.equal(response.headers.get("access-control-allow-origin"), null);
    }
  }
});

test("UI/Controller provenance matches target/instance/Host but only Controller may attest queue sender", async (t) => {
  const server = await fixture(t);
  const ui = { "x-treer-interface-source": "proxy-ui", "x-treer-agent-id": "ag_security", "x-treer-interface-instance": "security-instance", origin: "https://proxy.fixture.invalid", "sec-fetch-mode": "cors" };
  assert.equal((await fetch(server.base + "/api/state", { headers: ui })).status, 200);
  for (const headers of [{ ...ui, "x-treer-agent-id": "other" }, { ...ui, "x-treer-interface-instance": "stale" }, { ...ui, "x-treer-interface-source": "forged" }, { ...ui, host: "evil.invalid" }, { ...ui, host: "127.0.0.1:1" }, { ...ui, host: "agent-interface" }, { ...ui, "x-treer-interface-source": "controller-api" }]) assert.equal((await fetch(server.base + "/api/state", { headers })).status, 403);
  const body = { text: "fixture", operation_id: "fixture-op", sender: { agent_id: "ag_sender", path: "/sender", name: "sender" }, notify: { on: [] } };
  assert.equal((await fetch(server.base + "/v1/prompts", { method: "POST", headers: ui, body: JSON.stringify(body) })).status, 403);
  const controller = { ...ui, "x-treer-interface-source": "controller-api", host: "agent-interface" } as Record<string, string>;
  delete controller.origin;
  assert.equal((await fetch(server.base + "/v1/prompts", { method: "POST", headers: controller, body: JSON.stringify(body) })).status, 202);
  delete controller["x-treer-interface-source"];
  const legacy = await fetch(server.base + "/v1/prompts", { method: "POST", headers: controller, body: JSON.stringify(body) });
  assert.equal(legacy.status, 403); assert.match(await legacy.text(), /update Proxy\/Controller/);
});

test("WebSocket upgrade rejects hostile origins/Hosts/stale markers and accepts registered UI", async (t) => {
  const server = await fixture(t), uri = server.base.replace("http", "ws") + "/ws";
  const ui = { "x-treer-interface-source": "proxy-ui", "x-treer-agent-id": "ag_security", "x-treer-interface-instance": "security-instance", origin: "https://proxy.fixture.invalid" };
  for (const headers of [{ origin: "https://attacker.invalid" }, { "sec-fetch-mode": "websocket" }, { host: "evil.invalid" }, { ...ui, "x-treer-interface-instance": "stale" }]) {
    const status = await new Promise<number>((done, reject) => {
      const socket = new WebSocket(uri, { headers });
      socket.once("unexpected-response", (_, response) => { response.resume(); socket.terminate(); done(response.statusCode!); });
      socket.on("error", () => {}); socket.once("open", () => { socket.close(); reject(new Error("Untrusted WS accepted")); });
    });
    assert.equal(status, 403);
  }
  const socket = new WebSocket(uri, { headers: ui });
  const message = await new Promise<string>((done, reject) => { socket.once("message", (data) => done(String(data))); socket.once("error", reject); });
  assert.equal(JSON.parse(message).type, "state"); socket.close();
});
