// Repro only: sealed fake backend and synthetic data, never a managed/real Agent.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { createServer } from "node:http";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { chromium } from "@playwright/test";
import { hermeticEnv } from "./hermetic.mjs";

const root = resolve(import.meta.dirname, ".."), cwd = await mkdtemp(join(tmpdir(), "launcher-audit-"));
const portFile = join(cwd, "port");
await writeFile(join(cwd, ".env"), "SYNTHETIC_SECRET_NOT_A_CREDENTIAL");
const launcher = spawn(process.execPath, ["--import", "tsx", "src/index.ts"], { cwd: root,
  env: hermeticEnv({ HOME: cwd, CODEX_HOME: join(cwd, "codex"), CLAUDE_CONFIG_DIR: join(cwd, "claude"), AGENT_UI_CWD: cwd, AGENT_UI_PORT_FILE: portFile, AGENT_UI_PORT: "0", FAKE_CODEX_CALLS: join(cwd, "calls"), CODEX_BIN: join(root, "tests/fake-codex.mjs") }), stdio: "ignore" });
const attacker = createServer((_, response) => { response.writeHead(200, { "content-type": "text/html" }); response.end("<!doctype html><title>Local audit fixture</title>"); });
let browser;
try {
  let base;
  for (let i = 0; i < 100; i++) {
    try { base = `http://127.0.0.1:${Number(await readFile(portFile, "utf8"))}`; if ((await fetch(base + "/api/health")).ok) break; } catch { /* startup */ }
    await new Promise((done) => setTimeout(done, 20));
  }
  assert.ok(base);
  await new Promise((done) => attacker.listen(0, "127.0.0.1", done));
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage(); await page.goto(`http://127.0.0.1:${attacker.address().port}`);
  const evidence = await page.evaluate(async (base) => {
    const wsRead = await new Promise((done) => {
      const socket = new WebSocket(base.replace("http", "ws") + "/ws");
      const timeout = setTimeout(() => { socket.close(); done(false); }, 2000);
      socket.onmessage = (event) => { clearTimeout(timeout); socket.close(); done(JSON.parse(event.data).type === "state"); };
      socket.onerror = () => { clearTimeout(timeout); done(false); };
    });
    await fetch(base + "/api/prompt", { method: "POST", mode: "no-cors", headers: { "content-type": "text/plain" }, body: JSON.stringify({ prompt: "LOCAL_AUDIT_PROMPT_ONLY" }) });
    return { crossOriginWebSocketStateRead: wsRead };
  }, base);
  const calls = (await readFile(join(cwd, "calls"), "utf8")).trim().split("\n").map((line) => JSON.parse(line));
  evidence.crossOriginSimplePostReachedTurn = calls.some((call) => call.method === "turn/start" && JSON.stringify(call.params).includes("LOCAL_AUDIT_PROMPT_ONLY"));
  const rebinding = await fetch(base + "/v1/files?path=.env", { headers: { host: "rebinding.invalid" } });
  evidence.foreignHostReadSyntheticProjectFile = rebinding.status === 200 && (await rebinding.json()).content === "SYNTHETIC_SECRET_NOT_A_CREDENTIAL";
  await page.goto(base);
  evidence.sameOriginBrowserPassesArchiveHostOriginGuard = await page.evaluate(async () => (await fetch("/api/session/export")).status === 400);
  // Fake Codex has no valid native UUID: adapter validation returns 400 after the guard.
  console.log(JSON.stringify(evidence)); // Booleans only; never emit transcript/file contents.
} finally {
  await browser?.close(); await new Promise((done) => attacker.close(done));
  if (launcher.exitCode === null && launcher.signalCode === null) { launcher.kill("SIGTERM"); await once(launcher, "exit"); }
  await rm(cwd, { recursive: true, force: true });
}
