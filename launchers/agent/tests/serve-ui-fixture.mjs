#!/usr/bin/env node
// Starts the real launcher server with a fake Codex and rich sample history so the UI can be looked at in a browser.
//   node tests/serve-ui-fixture.mjs [--port 8099]      then open http://127.0.0.1:8099/ui.html?look=b
// No inference, no Treer mutations; the working directory is a temporary folder that is removed on exit.
// Browsers may only reach a launcher through Treer's Proxy, which adds identity headers; a tiny stand-in proxy does the same here.
import { spawn } from "node:child_process";
import { createServer, request as httpRequest } from "node:http";
import { connect } from "node:net";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { hermeticEnv } from "./hermetic.mjs";

const app = resolve(import.meta.dirname, "..");
const portIndex = process.argv.indexOf("--port");
const port = portIndex > 0 ? process.argv[portIndex + 1] : "8099";
const upstream = String(Number(port) + 1);
const cwd = await mkdtemp(join(tmpdir(), "ui-fixture-"));
await mkdir(join(cwd, "src"));
await writeFile(join(cwd, "src/greeting.ts"), "export const greeting = 'Hello, Treer';\n");
await writeFile(join(cwd, "README.md"), "# Workspace\nSmall tools, thoughtful interfaces.\n");
const env = hermeticEnv({ TREER_AGENT_ID: "ag_ui_fixture", AGENT_UI_INSTANCE_ID: "ui-fixture", CODEX_UI_CWD: cwd, CODEX_UI_PUBLIC_DIR: join(app, "public"), CODEX_UI_PORT: upstream,
  CODEX_BIN: join(app, "tests/fake-codex.mjs"), FAKE_CODEX_UX: "1", FAKE_CODEX_RICH: "1", FAKE_CODEX_MANY: "80" });
const server = spawn(join(app, "node_modules/.bin/tsx"), [join(app, "src/index.ts")], { env, stdio: "inherit" });
const identity = { "x-treer-interface-source": "proxy-ui", "x-treer-agent-id": "ag_ui_fixture", "x-treer-interface-instance": "ui-fixture" };
const proxy = createServer((req, res) => {
  const forward = httpRequest({ host: "127.0.0.1", port: upstream, path: req.url, method: req.method, headers: { ...req.headers, host: `127.0.0.1:${upstream}`, ...identity } }, (reply) => { res.writeHead(reply.statusCode ?? 502, reply.headers); reply.pipe(res); });
  forward.on("error", () => { res.writeHead(502); res.end("launcher not ready"); });
  req.pipe(forward);
});
proxy.on("upgrade", (req, socket, head) => {
  const back = connect(Number(upstream), "127.0.0.1", () => {
    const headers = { ...req.headers, host: `127.0.0.1:${upstream}`, ...identity };
    back.write(`${req.method} ${req.url} HTTP/1.1\r\n${Object.entries(headers).map(([name, value]) => `${name}: ${value}`).join("\r\n")}\r\n\r\n`);
    back.write(head);
    socket.pipe(back).pipe(socket);
  });
  back.on("error", () => socket.destroy());
  socket.on("error", () => back.destroy());
});
proxy.listen(Number(port), "127.0.0.1");
const stop = async () => { server.kill("SIGTERM"); await rm(cwd, { recursive: true, force: true }); process.exit(0); };
process.on("SIGINT", stop);
process.on("SIGTERM", stop);
server.on("exit", async () => {
  proxy.close(); await rm(cwd, { recursive: true, force: true }); process.exit(0); });
console.log(`UI fixture: http://127.0.0.1:${port}/ui.html?look=b`);
