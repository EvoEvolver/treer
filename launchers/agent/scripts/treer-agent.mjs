#!/usr/bin/env node
// Cross-platform entry point; resolve physical assets exactly once per process.
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, realpathSync, rmSync } from "node:fs";
import { get } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { compatibility } from "./bundle-contract.mjs";
import { runProviderSync, spawnProvider, stopProcessTree } from "./process.mjs";

const root = realpathSync(join(dirname(fileURLToPath(import.meta.url)), ".."));
const env = { ...process.env, AGENT_UI_APP_ROOT: root };
let harness = env.AGENT_UI_HARNESS || "codex", acpArgs = [];
const mapping = { "--session-id": "AGENT_UI_SESSION_ID", "--thread-id": "AGENT_UI_SESSION_ID", "--cwd": "AGENT_UI_CWD", "--acp-command": "AGENT_UI_ACP_COMMAND", "--name": "AGENT_UI_NAME" };
for (let i = 2; i < process.argv.length; i += 2) {
  const key = process.argv[i], value = process.argv[i + 1];
  if (value === undefined || key !== "--acp-arg" && !value) throw new Error("Incomplete launcher argument");
  if (key === "--harness") harness = value;
  else if (key === "--acp-arg") acpArgs.push(value);
  else if (mapping[key]) env[mapping[key]] = value;
  else throw new Error(`Unknown launcher argument: ${key}`);
}
if (!["codex", "claude", "acp"].includes(harness) || harness === "acp" && !env.AGENT_UI_ACP_COMMAND) throw new Error("Select codex, claude, or acp with --acp-command");
if (Number(process.versions.node.split(".")[0]) < 22) throw new Error("Node.js 22+ is required");
env.AGENT_UI_HARNESS = harness; env.AGENT_UI_ACP_ARGS = JSON.stringify(acpArgs);
env.AGENT_UI_CWD ||= env.CODEX_UI_CWD || process.cwd();
env.AGENT_UI_INSTANCE_ID ||= `agent-ui-${harness}-${env.TREER_AGENT_ID || "local"}-${process.pid}`;
env.AGENT_UI_PUBLIC_DIR ||= join(root, "public");
const temporary = mkdtempSync(join(tmpdir(), "treer-agent-port-"));
env.AGENT_UI_PORT_FILE = join(temporary, "port");
const cli = env.AGENT_UI_TREER_BIN || "treer";
const contract = JSON.parse(readFileSync(join(root, "interface-contract.json"), "utf8"));
try {
  const response = runProviderSync(cli, ["interface", "contract"], { env, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], timeout: 15000 });
  if (response.status !== 0) throw new Error("Controller does not expose the launcher compatibility handshake; update Controller");
  const aligned = compatibility(contract, JSON.parse(response.stdout));
  if (aligned.status !== "compatible") throw new Error(aligned.reason);
} catch (error) { rmSync(temporary, { recursive: true, force: true }); throw new Error(error.message); }
const portable = join(root, "runtime.mjs");
const args = existsSync(portable) ? [portable] : ["--import", join(root, "node_modules/tsx/dist/loader.mjs"), join(root, "src/index.ts")];
const child = spawnProvider(process.execPath, args, { env, stdio: "inherit" });
let ended = false, registered = false, cleaned = false;
function cleanup() {
  if (cleaned) return; cleaned = true;
  if (registered) runProviderSync(cli, ["interface", "clear"], { env, stdio: "ignore", timeout: 5000 });
  stopProcessTree(child); rmSync(temporary, { recursive: true, force: true });
}
child.on("error", () => { ended = true; process.exitCode = 1; cleanup(); });
child.on("exit", (code, signal) => { ended = true; process.exitCode = code ?? (signal ? 1 : 0); cleanup(); });
for (const signal of ["SIGINT", "SIGTERM"]) process.on(signal, () => { cleanup(); });
try {
  let port;
  const deadline = Date.now() + 90000;
  while (!ended && Date.now() < deadline) {
    if (existsSync(env.AGENT_UI_PORT_FILE)) {
      port = Number(readFileSync(env.AGENT_UI_PORT_FILE, "utf8"));
      if (Number.isInteger(port) && port > 0 && port <= 65535) {
        const healthy = await new Promise((done) => {
          const request = get(`http://127.0.0.1:${port}/api/health`, (response) => { response.resume(); done(response.statusCode === 200); });
          request.on("error", () => done(false)); request.setTimeout(2000, () => { request.destroy(); done(false); });
        });
        if (healthy) break;
      }
    }
    await new Promise((done) => setTimeout(done, 100));
  }
  if (ended || !port || Date.now() >= deadline) throw new Error("Agent runtime did not become ready");
  const registration = spawnSync(process.execPath, [join(root, "scripts/register-interface.mjs"), String(port)], { env, stdio: "inherit" });
  if (registration.status !== 0) throw new Error("Agent interface registration failed");
  registered = true;
} catch (error) { console.error(error.message); process.exitCode = 1; cleanup(); }
