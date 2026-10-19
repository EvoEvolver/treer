// Shared local STAND-IN for the acceptance scripts' self-tests (not Treer's Proxy or Console): a "console" origin
// (127.0.0.1) whose agent page frames the real launcher through an "agent origin" (localhost) that authenticates with a
// session cookie and adds frame-ancestors for the console. It does NOT model Treer's nonce/bootstrap handoff.
import { spawn } from "node:child_process";
import { createServer, request as httpRequest } from "node:http";
import { connect } from "node:net";
import { randomUUID } from "node:crypto";
import { mkdtemp, mkdir, writeFile, readFile, rm, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { hermeticEnv, localFetch } from "../hermetic.mjs";
import { fakePrivateState } from "../fake-private-state.mjs";

export const app = resolve(import.meta.dirname, "../..");
export const agentId = "ag_acceptance";
export const workspacePath = "/orgs/org_a/workspaces/ws_a";
export const cleanup = [];

export async function startLauncher(harness = "codex") {
  const cwd = await realpath(await mkdtemp(join(tmpdir(), "accept-launcher-")));
  const privateRoot = await realpath(await mkdtemp(join(tmpdir(), "accept-private-")));
  const controller = await fakePrivateState(agentId);
  await mkdir(join(cwd, "src")); await writeFile(join(cwd, "README.md"), "# fixture\n");
  const common = { UI_DEV_FIXTURE: randomUUID(), TREER_AGENT_PRIVATE_STATE_DIR: privateRoot, TREER_AGENT_ID: agentId, TREER_AGENT_SERVER_URL: controller.url, TREER_WORKLOAD_CREDENTIAL: "fixture-only", AGENT_UI_MESSAGE_WATCH: "0", AGENT_UI_INSTANCE_ID: "acceptance" };
  // The Claude harness reads AGENT_UI_*; the Codex one CODEX_UI_* (same split as tests/browser-ui.mjs).
  const env = hermeticEnv(harness === "claude"
    ? { ...common, AGENT_UI_HARNESS: "claude", AGENT_UI_CWD: cwd, AGENT_UI_PORT: "0", AGENT_UI_PORT_FILE: join(cwd, "port"), AGENT_UI_PUBLIC_DIR: join(app, "public"), CLAUDE_BIN: join(app, "tests/fake-claude.mjs") }
    : { ...common, CODEX_UI_PUBLIC_DIR: join(app, "public"), CODEX_UI_PORT: "0", CODEX_UI_CWD: cwd, CODEX_UI_PORT_FILE: join(cwd, "port"), CODEX_BIN: join(app, "tests/fake-codex.mjs"), FAKE_CODEX_COMPLETE_TURNS: "1" });
  const child = spawn(join(app, "node_modules/.bin/tsx"), [join(app, "src/index.ts")], { env, stdio: "pipe" });
  let logs = ""; child.stderr.on("data", (c) => { logs += c; }); child.stdout.on("data", (c) => { logs += c; });
  cleanup.push(async () => { if (child.exitCode === null) { const exited = new Promise((r) => child.once("exit", r)); child.kill("SIGTERM"); await exited; } await controller.close(); await rm(cwd, { recursive: true, force: true }); await rm(privateRoot, { recursive: true, force: true }); });
  for (let attempt = 0; attempt < 200; attempt++) {
    if (child.exitCode !== null) throw new Error(logs);
    try { const port = Number(await readFile(join(cwd, "port"), "utf8")); if ((await localFetch(`http://127.0.0.1:${port}/api/health`)).ok) return port; } catch { /* starting */ }
    await new Promise((r) => setTimeout(r, 50));
  }
  throw new Error(logs);
}

export const listen = (server) => new Promise((ok) => server.listen(0, "127.0.0.1", () => { cleanup.push(() => new Promise((r) => server.close(r))); ok(server.address().port); }));

// mode: good | no-frame-ancestors | anonymous-open | blank-embed | shared-origin
export async function startStandIn(launcherPort, mode, seenLanguages = []) {
  let consolePort = 0, agentPort = 0;
  const authorized = (request) => mode === "anonymous-open" || /(?:^|; )sess=ok(?:;|$)/.test(request.headers.cookie || "");
  const agent = createServer((request, response) => {
    if (request.headers["sec-fetch-dest"] === "document" || request.headers["sec-fetch-dest"] === "iframe") seenLanguages.push(String(request.headers["accept-language"] || ""));
    if (mode === "standalone-403" && request.headers["sec-fetch-dest"] === "document") { response.writeHead(403, { "content-type": "text/plain" }); response.end("forbidden"); return; }
    if (!authorized(request)) {
      if (mode === "anonymous-silent") { response.writeHead(200, { "content-type": "text/html" }); response.end("<p>nothing here</p>"); return; }
      response.writeHead(401, { "content-type": "text/plain" }); response.end("no session"); return;
    }
    const forwarded = httpRequest({ hostname: "127.0.0.1", port: launcherPort, path: request.url, method: request.method, headers: { ...request.headers, host: `127.0.0.1:${launcherPort}`, "x-treer-interface-source": "proxy-ui", "x-treer-agent-id": agentId, "x-treer-interface-instance": "acceptance" } }, (upstream) => {
      const headers = { ...upstream.headers };
      if (mode !== "no-frame-ancestors") headers["content-security-policy"] = [headers["content-security-policy"], `frame-ancestors ${mode === "wide-ancestors" ? "*" : `http://127.0.0.1:${consolePort}`}`].filter(Boolean).join("; ");
      response.writeHead(upstream.statusCode, headers); upstream.pipe(response);
    });
    forwarded.on("error", () => { response.writeHead(502); response.end(); });
    request.pipe(forwarded);
  });
  agent.on("upgrade", (request, socket, head) => {
    // Browsers do not send a Secure cookie on ws:// in every engine; the page itself only loads with the session, so
    // accept the socket when its Origin is the agent origin (a header a foreign page cannot forge).
    if (!authorized(request) && request.headers.origin !== `http://localhost:${agentPort}`) { socket.destroy(); return; }
    const upstream = connect(launcherPort, "127.0.0.1", () => {
      const headers = { ...request.headers, host: `127.0.0.1:${launcherPort}`, "x-treer-interface-source": "proxy-ui", "x-treer-agent-id": agentId, "x-treer-interface-instance": "acceptance" };
      upstream.write(`${request.method} ${request.url} HTTP/1.1\r\n${Object.entries(headers).map(([k, v]) => `${k}: ${v}`).join("\r\n")}\r\n\r\n`); upstream.write(head);
      socket.pipe(upstream).pipe(socket);
    });
    upstream.on("error", () => socket.destroy()); socket.on("error", () => upstream.destroy());
  });
  const consoleServer = createServer((request, response) => {
    const path = new URL(request.url, "http://x").pathname;
    if (path === `${workspacePath}/workloads/${agentId}`) {
      const src = mode === "shared-origin" ? "/about-blank" : mode === "blank-embed" ? "http://localhost:1/" : `http://localhost:${agentPort}/`;
      response.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      response.end(`<!doctype html><title>Console</title>${mode === "probe-text-outside" ? '<script>setTimeout(() => { throw new Error("Blocked a frame with origin \\"https://a.example\\" from accessing a frame with origin \\"https://b.example\\""); }, 0);</script>' : ""}${mode === "probe-logs" ? '<script>Object.defineProperty(HTMLIFrameElement.prototype, "contentDocument", { get() { setTimeout(() => { throw new Error("Blocked a frame with origin \\"https://a.example\\" from accessing a frame with origin \\"https://b.example\\""); }, 0); return null; } });</script>' : ""}${mode === "beacon" ? '<script src="https://static.cloudflareinsights.com/beacon.min.js" defer></script>' : ""}<iframe title="Alpha interface" src="${src}" style="width:100%;height:90vh;border:0"></iframe>`);
    } else if (path === "/about-blank") { response.writeHead(200, { "content-type": "text/html" }); response.end("<p>compat</p>"); }
    else { response.writeHead(404); response.end(); }
  });
  agentPort = await listen(agent); consolePort = await listen(consoleServer);
  return { consolePort, agentPort };
}

export async function writeState(path) {
  await writeFile(path, JSON.stringify({ cookies: [{ name: "sess", value: "ok", domain: "localhost", path: "/", expires: -1, httpOnly: true, secure: true, sameSite: "None" }], origins: [] }), { mode: 0o600 });
}

