import { createReadStream, existsSync, readFileSync, realpathSync, statSync, writeFileSync } from "node:fs";
import { createServer, type ServerResponse } from "node:http";
import { extname, join, normalize, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

import { WebSocketServer, type WebSocket } from "ws";

import { AcpRuntime } from "./harness/acp.js";
import { CodexRuntime } from "./harness/codex.js";
import { ClaudeRuntime } from "./harness/claude.js";
import type { HarnessRuntime } from "./runtime.js";
import { threadStatePath } from "./thread-state.js";
import { listFiles, previewFile, fileErrorStatus } from "./files.js";
import { InvalidPromptMode, PromptQueue, promptMode } from "./prompt-queue.js";
import { taskOutput } from "./task-output.js";
import { GoalError, goalUpdate } from "./goals.js";
import { QueueError, submissionOptions } from "./prompt-submission.js";
import { notifySubmission } from "./prompt-notify.js";
import { MessageWatch, messageWatchEnabled } from "./message-watch.js";
import { guardInterfaceRequest, verifyInterfaceCaller } from "./interface-identity.js";
import { SessionTransferError } from "./session-transfer.js";
import { HttpInputError, readJson } from "./http-input.js";
import { GoalNotifications } from "./goal-notify.js";
import { GoalExitRequests } from "./goal-exit.js";
import { GoalEdits } from "./goal-edits.js";

/** Reads AGENT_UI_<name>, falling back to the CODEX_UI_<name> spelling of older launches. */
function setting(name: string) {
  return process.env[`AGENT_UI_${name}`] ?? process.env[`CODEX_UI_${name}`];
}

const appRoot = resolve(setting("APP_ROOT") || fileURLToPath(new URL("..", import.meta.url)));
const publicDir = resolve(setting("PUBLIC_DIR") || join(appRoot, "public"));
const port = Number(setting("PORT") ?? "0");
if (!Number.isInteger(port) || port < 0 || port > 65535) throw new Error("AGENT_UI_PORT must be a port from 0 to 65535");
const cwd = resolve(setting("CWD") || process.cwd());
const agentId = process.env.TREER_AGENT_ID || "";
const interfaceInstanceId = setting("INSTANCE_ID") || `agent-ui-${process.pid}`;
const interfaceContract = JSON.parse(readFileSync(join(appRoot, "interface-contract.json"), "utf8")) as { schema: string; protocol: string; capabilities: string[]; ui_path: string };
if (interfaceContract.schema !== "treer.launcher-interface/v1" || interfaceContract.protocol !== "treer.agent-interface/v1" || !Array.isArray(interfaceContract.capabilities)) throw new Error("Unsupported launcher interface contract");
const interfaceCapabilities = interfaceContract.capabilities;
const initialHistoryLimit = 50;

function createRuntime(): HarnessRuntime {
  const harness = process.env.AGENT_UI_HARNESS || "codex";
  const resumeId = process.env.AGENT_UI_SESSION_ID || process.env.CODEX_UI_THREAD_ID;
  const stateFile = threadStatePath(cwd, agentId);
  if (harness === "codex") {
    return new CodexRuntime(process.env.CODEX_BIN || "codex", cwd, { threadId: resumeId, stateFile });
  }
  if (harness === "claude") {
    return new ClaudeRuntime(process.env.CLAUDE_BIN || "claude", cwd, { sessionId: resumeId, stateFile, model: process.env.CLAUDE_MODEL });
  }
  if (harness === "acp") {
    const command = process.env.AGENT_UI_ACP_COMMAND;
    if (!command) throw new Error("AGENT_UI_ACP_COMMAND is required for the acp harness");
    const args = JSON.parse(process.env.AGENT_UI_ACP_ARGS || "[]") as unknown;
    if (!Array.isArray(args) || !args.every((arg) => typeof arg === "string")) throw new Error("AGENT_UI_ACP_ARGS must be a JSON string array");
    return new AcpRuntime(command, args, cwd, { sessionId: resumeId, stateFile, name: process.env.AGENT_UI_NAME });
  }
  throw new Error(`Unknown AGENT_UI_HARNESS ${harness}; expected codex, claude or acp`);
}

const runtime = createRuntime();
const sockets = new Set<{ socket: WebSocket; historyLimit: number }>();
if (agentId && !/^[A-Za-z0-9_-]+$/.test(agentId)) throw new Error("Invalid TREER_AGENT_ID for queue storage");
const prompts = new PromptQueue(runtime, broadcast, {
  file: join(realpathSync(cwd), ".treer", "agents", agentId || "standalone", "queue.json"),
  limit: Number(setting("QUEUE_LIMIT") ?? "256"),
  notify: notifySubmission,
});

function mime(file: string) {
  switch (extname(file)) {
    case ".html": return "text/html; charset=utf-8";
    case ".js": return "text/javascript; charset=utf-8";
    case ".css": return "text/css; charset=utf-8";
    case ".json": return "application/json; charset=utf-8";
    case ".svg": return "image/svg+xml";
    case ".png": return "image/png";
    case ".woff2": return "font/woff2";
    default: return "application/octet-stream";
  }
}

function send(response: ServerResponse, status: number, body: unknown) {
  const payload = typeof body === "string" || Buffer.isBuffer(body) ? body : JSON.stringify(body);
  response.writeHead(status, {
    "cache-control": "no-store",
    "content-type": "application/json; charset=utf-8",
  });
  response.end(payload);
}

function serveFile(response: ServerResponse, file: string) {
  if (!existsSync(file) || !statSync(file).isFile()) return false;
  const headers: Record<string, string> = {
    "cache-control": "no-store",
    "content-type": mime(file),
  };
  if (extname(file) === ".html") {
    headers["content-security-policy"] =
      "default-src 'self'; connect-src 'self'; img-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; script-src 'self'; font-src 'self' data:";
  }
  response.writeHead(200, headers);
  createReadStream(file).pipe(response);
  return true;
}

function requestPath(url: URL) {
  return url.pathname.replace(/\/+$/, "") || "/";
}

function historyLimitFrom(body: Record<string, unknown>) {
  return typeof body.history_limit === "number" && Number.isFinite(body.history_limit)
    ? Math.min(10_000, Math.max(initialHistoryLimit, Math.floor(body.history_limit)))
    : initialHistoryLimit;
}

function statePayload(historyLimit = initialHistoryLimit) {
  const snapshot = runtime.snapshot(historyLimit);
  return {
    ready: snapshot.ready,
    runtime: {
      state: !snapshot.ready
        ? "starting"
        : snapshot.thread?.status === "running"
          ? "working"
          : snapshot.thread?.status === "error"
            ? "blocked"
            : "idle",
      error: snapshot.thread?.lastError ?? null,
    },
    harness: runtime.harness,
    thread: snapshot.thread,
    modelOptions: snapshot.models,
    backgroundTasks: snapshot.backgroundTasks,
    queue: prompts.entries,
    goal: snapshot.goal,
    goalDelivery: goalEdits.state,
    history: { limit: historyLimit },
  };
}

function transcriptPayload(url: URL) {
  const turns = runtime.snapshot().thread?.turns ?? [];
  const pageRaw = url.searchParams.get("page") ?? url.searchParams.get("cursor") ?? "0";
  const limitRaw = url.searchParams.get("limit") ?? "1";
  const page = Math.max(0, Number.parseInt(pageRaw, 10) || 0);
  const limit = Math.min(1000, Math.max(1, Number.parseInt(limitRaw, 10) || 1));
  const selected = turns.slice(page, page + limit);
  const nextPage = page + selected.length < turns.length ? page + selected.length : null;
  return {
    agent_id: agentId,
    interface_instance_id: interfaceInstanceId,
    page,
    page_count: turns.length,
    next_page: nextPage,
    cursor: String(page),
    next_cursor: nextPage == null ? null : String(nextPage),
    entries: selected.flatMap((turn) =>
      turn.items.map((item, index) => ({
        id: item.id || `${turn.id}:${index}`,
        kind: item.kind,
        role: item.kind === "userMessage" ? "user" : item.kind === "agentMessage" ? "assistant" : null,
        content: item.text,
        created_at: turn.startedAt,
      })),
    ),
  };
}

function broadcast() {
  for (const client of sockets) {
    try {
      client.socket.send(JSON.stringify({ type: "state", ...statePayload(client.historyLimit) }));
    } catch {
      sockets.delete(client);
    }
  }
}

runtime.on("state", broadcast);
runtime.on("log", (message) => console.log(`[${runtime.harness.id}] ${message}`));

const server = createServer(async (request, response) => {
  const method = request.method ?? "GET";
  try {
    let url: URL;
    try { url = new URL(request.url ?? "/", "http://127.0.0.1"); }
    catch { throw new HttpInputError("Invalid request URL", 400); }
    const path = requestPath(url);
    const address = server.address();
    guardInterfaceRequest(request.headers, agentId, interfaceInstanceId, typeof address === "object" && address ? address.port : port);
    const queueRoute = path === "/v1/prompts" || path === "/v1/queue" || path.startsWith("/v1/queue/") || path.startsWith("/v1/submissions/");
    const attested = queueRoute ? verifyInterfaceCaller(request.headers, agentId, interfaceInstanceId) : false;
    if (path === "/api/session/export" && method === "GET") {
      // A restrictive Host/Origin heuristic, not client authentication: same-
      // origin browser GETs can omit Origin, and native clients can forge Host.
      const host = request.headers.host ?? "";
      if (request.headers.origin !== undefined || !/^(?:127\.0\.0\.1|localhost|\[::1\])(?::\d+)?$/i.test(host)) {
        send(response, 403, { error: "Session export requires loopback Host and no Origin; use session.sh for offline export" });
        return;
      }
      if (!runtime.exportSession) throw new SessionTransferError("unsupported", "This harness does not support session export");
      send(response, 200, await runtime.exportSession());
      return;
    }
    if (path === "/v1/files/tree" || path === "/v1/files") {
      if (method !== "GET") {
        response.setHeader("allow", "GET");
        send(response, 405, { error: { code: "method_not_allowed", message: "File browsing is read-only; use GET" } });
        return;
      }
      try {
        const filePath = url.searchParams.get("path") ?? "";
        send(response, 200, path === "/v1/files/tree" ? await listFiles(cwd, filePath) : await previewFile(cwd, filePath));
      } catch (error) {
        const status = fileErrorStatus(error);
        send(response, status, { error: { code: "file_access_failed", message: status === 404 ? "File or directory not found" : status === 500 ? "Unable to read file or directory" : error instanceof Error ? error.message : "File access denied" } });
      }
      return;
    }
    if (path === "/v1/manifest" && (method === "GET" || method === "HEAD")) {
      send(response, 200, {
        protocol: interfaceContract.protocol,
        instance_id: interfaceInstanceId,
        capabilities: interfaceCapabilities,
        ui_path: interfaceContract.ui_path,
      });
      return;
    }
    if (path === "/v1/health" && (method === "GET" || method === "HEAD")) {
      const snapshot = runtime.snapshot();
      send(response, snapshot.ready ? 200 : 503, {
        instance_id: interfaceInstanceId,
        status: snapshot.ready ? "ok" : "starting",
      });
      return;
    }
    if (path === "/v1/status" && method === "GET") {
      const payload = statePayload();
      send(response, 200, {
        agent_id: agentId,
        interface_instance_id: interfaceInstanceId,
        status: payload.runtime.state,
        busy: prompts.busy,
        queue_length: prompts.entries.length,
        error: payload.runtime.error,
      });
      return;
    }
    if (path === "/v1/transcript" && method === "GET") {
      send(response, 200, transcriptPayload(url));
      return;
    }
    if (path === "/v1/prompts" && method === "POST") {
      const body = await readJson(request);
      const operationId = typeof body.operation_id === "string" ? body.operation_id.trim() : "";
      const text = typeof body.text === "string" ? body.text.trim() : "";
      if (!operationId || !text) {
        send(response, 400, { error: "operation_id and text are required" });
        return;
      }
      // "mode": "prompt" is the Core compatibility field. Never reinterpret it.
      if (!attested && (body.sender != null || body.notify != null)) throw new QueueError("Sender and notifications require an authenticated managed Controller request", 403);
      const metadata = submissionOptions(body.sender, body.notify);
      const submission = await prompts.submit(text, promptMode(body.delivery, "queue"), { operationId, ...metadata });
      send(response, 202, { accepted: true, operation_id: operationId, ...(submission.duplicate ? { duplicate: true } : {}), submission });
      return;
    }
    if (path === "/v1/queue" && method === "GET") {
      send(response, 200, { submissions: prompts.submissions }); return;
    }
    const submissionMatch = /^\/v1\/submissions\/([^/]+)$/.exec(path);
    if (submissionMatch && method === "GET") {
      const submission = prompts.submission(decodeURIComponent(submissionMatch[1]));
      send(response, submission ? 200 : 404, submission ? { submission } : { error: "Submission not found" }); return;
    }
    const cancelMatch = /^\/v1\/queue\/([^/]+)$/.exec(path);
    if (cancelMatch && method === "DELETE") {
      const id = decodeURIComponent(cancelMatch[1]);
      const removed = prompts.remove(id);
      send(response, removed ? 200 : 404, removed ? { submission: prompts.submission(id) } : { error: "Submission not found" }); return;
    }
    if (path === "/v1/abort" && method === "POST") {
      await runtime.interrupt();
      send(response, 202, { accepted: true });
      return;
    }
    if (path === "/api/health" || path === "/.treer/agent") {
      const payload = statePayload();
      send(response, payload.ready ? 200 : 503, path === "/api/health"
        ? { ok: payload.ready, ready: payload.ready }
        : {
            protocol: "treer.agent.surface",
            version: 1,
            ready: payload.ready,
            title: payload.thread?.title ?? runtime.harness.name,
            ui: true,
            capabilities: interfaceCapabilities,
          });
      return;
    }
    if ((path === "/api/state" || path === "/v1/state") && method === "GET") {
      send(response, 200, statePayload());
      return;
    }
    if (path === "/v1/goal/exit" && method === "POST") {
      if (process.env.AGENT_UI_GOAL_AGENT_EXIT === "0") throw new GoalError("Agent goal exits are disabled by AGENT_UI_GOAL_AGENT_EXIT=0", 403);
      if (!runtime.harness.capabilities.goal || !goalExits) throw new GoalError("Goal exits require a managed supported harness", 501);
      const update = goalUpdate(await readJson(request));
      if (!["blocked", "stale", "done"].includes(update.status || "")) throw new GoalError("An exit requires blocked, stale or done status");
      if (!runtime.snapshot().goal) throw new GoalError("No goal set", 409);
      send(response, 202, goalExits.enqueue(update)); return;
    }
    if (path === "/api/goal" && (method === "PUT" || method === "DELETE")) {
      if (!runtime.harness.capabilities.goal || !runtime.updateGoal || !runtime.clearGoal) throw new GoalError(`${runtime.harness.name} does not support goals`, 501);
      if (method === "DELETE") await runtime.clearGoal();
      else {
        const body = await readJson(request);
        const update = goalUpdate(body);
        if (["blocked", "stale", "done"].includes(update.status || "") && process.env.AGENT_UI_GOAL_AGENT_EXIT === "0") throw new GoalError("Agent goal exits are disabled by AGENT_UI_GOAL_AGENT_EXIT=0", 403);
        if (["blocked", "stale", "done"].includes(update.status || "")) await runtime.updateGoal(update);
        else await goalEdits.apply(update, typeof body.operationId === "string" ? body.operationId : undefined);
      }
      send(response, 200, statePayload());
      return;
    }
    if (path === "/api/goal/retry" && method === "POST") {
      const body = await readJson(request); goalEdits.retry(String(body.id || "")); send(response, 202, statePayload()); return;
    }
    if (path.startsWith("/api/queue/") && method === "DELETE") {
      const removed = prompts.remove(decodeURIComponent(path.slice("/api/queue/".length)));
      send(response, removed ? 200 : 404, removed ? statePayload() : { error: "Queued prompt not found" });
      return;
    }
    const taskMatch = /^\/api\/tasks\/([^/]+)\/output$/.exec(path);
    if (taskMatch && method === "GET") {
      const task = runtime.snapshot().backgroundTasks.find((entry) => entry.id === decodeURIComponent(taskMatch[1]));
      if (!task) { send(response, 404, { error: "Task not found" }); return; }
      try { send(response, 200, await taskOutput(task, runtime.taskOutputRoots ?? [cwd], url.searchParams.get("tail"))); }
      catch (error) { send(response, 400, { error: error instanceof Error ? error.message : String(error) }); }
      return;
    }
    if (path === "/api/history/older" && method === "POST") {
      const body = await readJson(request);
      const current = typeof body.limit === "number" && Number.isFinite(body.limit)
        ? Math.max(initialHistoryLimit, Math.floor(body.limit))
        : initialHistoryLimit;
      send(response, 200, statePayload(Math.min(current + initialHistoryLimit, 10_000)));
      return;
    }
    if (path === "/api/prompt" && method === "POST") {
      const body = await readJson(request);
      const prompt = typeof body.prompt === "string" ? body.prompt.trim() : "";
      if (!prompt) {
        send(response, 400, { error: "prompt is required" });
        return;
      }
      const submission = await prompts.submit(prompt, promptMode(body.mode));
      send(response, 200, { ...statePayload(historyLimitFrom(body)), submission });
      return;
    }
    if (path === "/api/interrupt" && method === "POST") {
      const body = await readJson(request);
      await runtime.interrupt();
      send(response, 200, statePayload(historyLimitFrom(body)));
      return;
    }
    if (path === "/api/settings" && method === "POST") {
      const body = await readJson(request);
      await runtime.updateSettings({
        model: typeof body.model === "string" ? body.model : undefined,
        reasoningEffort: body.reasoningEffort === undefined
          ? undefined
          : typeof body.reasoningEffort === "string" || body.reasoningEffort === null
            ? body.reasoningEffort
            : undefined,
      });
      send(response, 200, statePayload(historyLimitFrom(body)));
      return;
    }

    const relative = path === "/" ? "index.html" : path.slice(1);
    const file = normalize(join(publicDir, relative));
    if ((file === publicDir || file.startsWith(`${publicDir}${sep}`)) && serveFile(response, file)) return;
    if (method === "GET" && serveFile(response, join(publicDir, "index.html"))) return;
    send(response, 404, { error: "not found" });
  } catch (error) {
    // Never log parser exceptions (which can quote the request body).
    console.error(`Agent UI request failed: ${error instanceof Error ? error.name : "Error"}`);
    send(response, error instanceof HttpInputError || error instanceof GoalError || error instanceof QueueError ? error.status : error instanceof InvalidPromptMode ? 400 : error instanceof SessionTransferError ? error.code === "unsupported" ? 501 : error.code === "not_ready" ? 409 : error.code === "too_large" ? 413 : 400 : 500, { error: error instanceof Error ? error.message : "Request failed" });
  }
});

const socketsServer = new WebSocketServer({ noServer: true, maxPayload: 16 * 1024 });
server.on("upgrade", (request, socket, head) => {
  try {
    const address = server.address();
    guardInterfaceRequest(request.headers, agentId, interfaceInstanceId, typeof address === "object" && address ? address.port : port);
  } catch {
    socket.end("HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"); return;
  }
  let path: string;
  try { path = requestPath(new URL(request.url ?? "/", "http://127.0.0.1")); }
  catch { socket.destroy(); return; }
  if (path !== "/ws") {
    socket.destroy();
    return;
  }
  socketsServer.handleUpgrade(request, socket, head, (webSocket) => {
    const client = { socket: webSocket, historyLimit: initialHistoryLimit };
    sockets.add(client);
    webSocket.send(JSON.stringify({ type: "state", ...statePayload(client.historyLimit) }));
    webSocket.on("message", (data) => {
      try {
        const message = JSON.parse(String(data)) as { type?: string; limit?: number; respond?: boolean };
        if (message.type === "history-limit" && typeof message.limit === "number") {
          client.historyLimit = Math.min(10_000, Math.max(initialHistoryLimit, Math.floor(message.limit)));
          if (message.respond !== false) {
            webSocket.send(JSON.stringify({ type: "state", ...statePayload(client.historyLimit) }));
          }
        }
      } catch {
        // Ignore malformed client messages.
      }
    });
    webSocket.on("close", () => sockets.delete(client));
    webSocket.on("error", () => sockets.delete(client));
  });
});

const messageWatch = messageWatchEnabled(process.env) ? new MessageWatch(prompts, agentId, (message) => runtime.emit("log", message)) : undefined;
const goalNotifications = agentId ? new GoalNotifications(threadStatePath(cwd, agentId) + ".goal-notifications.json", (message) => runtime.emit("log", message)) : undefined;
const goalExits = agentId && process.env.AGENT_UI_GOAL_AGENT_EXIT !== "0" ? new GoalExitRequests(threadStatePath(cwd, agentId) + ".goal-exits.json", runtime, (message) => runtime.emit("log", message)) : undefined;
const goalEdits = new GoalEdits(agentId ? threadStatePath(cwd, agentId) + ".goal-edits.json" : undefined, runtime, prompts, broadcast);
runtime.on("goal-exit", (goal) => goalNotifications?.enqueue(goal));
runtime.on("goal-exit-error", (goal) => goalNotifications?.enqueue(goal));
runtime.on("state", () => {
  const goal = runtime.snapshot().goal;
  if (goal?.reason && ["blocked", "stale", "done"].includes(goal.status)) goalNotifications?.enqueue(goal);
});
runtime.on("exit", () => { void messageWatch?.stop(); prompts.close(); void runtime.stop().finally(() => process.exit(1)); });
server.listen(port, "127.0.0.1", () => {
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("Missing listener address");
  const portFile = setting("PORT_FILE");
  if (portFile) writeFileSync(portFile, String(address.port), { mode: 0o600 });
  process.env.AGENT_UI_URL = `http://127.0.0.1:${address.port}`;
  process.env.AGENT_UI_GOAL_COMMAND = join(appRoot, "scripts/goal.mjs");
  console.log(`Treer Agent UI (${runtime.harness.id}) listening on http://127.0.0.1:${address.port}`);
  runtime.start().then(() => messageWatch?.start()).catch((error) => {
    console.error(`failed to start ${runtime.harness.name}`, error);
    void runtime.stop().finally(() => process.exit(1));
  });
});

for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"] as const) {
  process.on(signal, () => {
    goalNotifications?.close();
    goalExits?.close();
    goalEdits.close();
    void messageWatch?.stop();
    prompts.close();
    void runtime.stop().finally(() => process.exit(0));
  });
}
