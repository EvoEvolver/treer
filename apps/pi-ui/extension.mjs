import { execFile } from "node:child_process";
import { randomBytes } from "node:crypto";
import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { dirname, extname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const extensionPath = fileURLToPath(import.meta.url);
const publicDir = join(dirname(extensionPath), "public");
const MAX_BODY_BYTES = 1024 * 1024;

const assetTypes = new Map([
  [".css", "text/css; charset=utf-8"],
  [".html", "text/html; charset=utf-8"],
  [".js", "text/javascript; charset=utf-8"],
]);

export function normalizePort(value) {
  const port = Number.parseInt(value ?? "4180", 10);
  if (!Number.isInteger(port) || port < 0 || port > 65535) {
    throw new Error(`PI_UI_PORT must be an integer from 0 to 65535, got ${value}`);
  }
  return port;
}

export function serviceName(agentId) {
  const suffix = String(agentId || "local")
    .replace(/[^a-zA-Z0-9_-]/g, "")
    .slice(-12) || "local";
  return `pi-ui-${suffix}`;
}

export function promptOptions(isIdle, mode) {
  if (isIdle) return undefined;
  if (mode === "steer") return { deliverAs: "steer" };
  if (mode === "followUp") return { deliverAs: "followUp" };
  throw new Error("Pi is working; choose steer or follow-up delivery");
}

export function forkAgentName(parentName, suffix) {
  const cleanSuffix = String(suffix).replace(/[^a-zA-Z0-9-]/g, "").slice(0, 24) || "new";
  const tail = `-fork-${cleanSuffix}`;
  const base = [...(String(parentName || "pi").trim() || "pi")]
    .slice(0, 80 - tail.length)
    .join("");
  return `${base}${tail}`;
}

export function forkLaunchArgs({ cwd, extension, name, sessionFile }) {
  return [
    "agent", "admin", "create",
    "--machine", "self",
    "--kind", "shell",
    "--name", name,
    "--cwd", cwd,
    "--",
    "env", "PI_UI_PORT=0",
    "pi", "--fork", sessionFile,
    "--extension", extension,
    "--approve",
  ];
}

export function canForkSession(context) {
  if (!context?.sessionManager.getSessionFile()) return false;
  return context.sessionManager.getBranch().some((entry) =>
    entry.type === "message"
    && (entry.message?.role === "user" || entry.message?.role === "assistant"));
}

function parseCommandJson(stdout, label) {
  try {
    return JSON.parse(String(stdout));
  } catch {
    throw new Error(`${label} returned invalid JSON`);
  }
}

export async function createForkedAgent(context, options = {}) {
  if (!context?.isIdle?.()) throw new Error("Wait for the current Pi response to finish before forking");
  if (!canForkSession(context)) throw new Error("Fork requires at least one saved conversation message");
  const sessionFile = context.sessionManager.getSessionFile();

  const run = options.run ?? execFileAsync;
  const parentResult = await run("treer", ["agent", "show", "self"]);
  const parent = parseCommandJson(parentResult.stdout, "treer agent show self");
  if (!parent.cwd || !parent.name) throw new Error("Treer returned incomplete parent Agent metadata");

  const suffix = options.suffix
    ?? `${Date.now().toString(36)}-${randomBytes(2).toString("hex")}`;
  const childName = forkAgentName(parent.name, suffix);
  const createResult = await run("treer", forkLaunchArgs({
    cwd: parent.cwd,
    extension: options.extension ?? extensionPath,
    name: childName,
    sessionFile,
  }));
  const child = parseCommandJson(createResult.stdout, "treer agent admin create");
  if (!child.agent_id || !child.name) throw new Error("Treer returned incomplete forked Agent metadata");
  return child;
}

export function snapshotFromContext(context, runtime) {
  return {
    agentId: process.env.TREER_AGENT_ID || null,
    cwd: context?.cwd ?? process.cwd(),
    session: context
      ? {
          id: context.sessionManager.getSessionId(),
          file: context.sessionManager.getSessionFile() ?? null,
          name: context.sessionManager.getSessionName() ?? null,
        }
      : null,
    model: context?.model
      ? {
          id: context.model.id,
          name: context.model.name,
          provider: context.model.provider,
        }
      : null,
    thinkingLevel: context?.thinkingLevel ?? null,
    contextUsage: context?.getContextUsage?.() ?? null,
    busy: runtime.busy,
    connected: Boolean(context),
    canFork: canForkSession(context),
    entries: context?.sessionManager.getBranch() ?? [],
    liveMessage: runtime.liveMessage,
    activeTools: [...runtime.activeTools.values()],
    error: runtime.error,
    forking: runtime.forking,
    lastFork: runtime.lastFork,
    port: runtime.port,
  };
}

function json(value) {
  return JSON.stringify(value, (_key, item) =>
    typeof item === "bigint" ? item.toString() : item,
  );
}

async function readBody(request) {
  let size = 0;
  const chunks = [];
  for await (const chunk of request) {
    size += chunk.length;
    if (size > MAX_BODY_BYTES) throw new Error("request body is too large");
    chunks.push(chunk);
  }
  if (chunks.length === 0) return {};
  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
}

function sendJson(response, status, value, headOnly = false) {
  const body = json(value);
  response.writeHead(status, {
    "cache-control": "no-store",
    "content-length": Buffer.byteLength(body),
    "content-type": "application/json; charset=utf-8",
  });
  response.end(headOnly ? undefined : body);
}

async function serveAsset(pathname, response, headOnly = false) {
  const file = pathname === "/" ? "index.html" : pathname.slice(1);
  if (!/^[a-zA-Z0-9._-]+$/.test(file)) return false;
  const contentType = assetTypes.get(extname(file));
  if (!contentType) return false;
  try {
    const body = await readFile(join(publicDir, file));
    response.writeHead(200, {
      "cache-control": "no-cache",
      "content-length": body.length,
      "content-security-policy": "default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'",
      "content-type": contentType,
      "referrer-policy": "no-referrer",
      "x-content-type-options": "nosniff",
    });
    response.end(headOnly ? undefined : body);
    return true;
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
}

async function registerTreerUi(port, name) {
  const createArgs = [
    "network", "service", "create", name,
    "--agent", "self", "--port", String(port), "--protocol", "http",
  ];
  try {
    await execFileAsync("treer", createArgs);
  } catch {
    await execFileAsync("treer", [
      "network", "service", "update", name,
      "--port", String(port), "--protocol", "http",
    ]);
  }
  await execFileAsync("treer", ["ui", "set", name]);
}

export default function piUiExtension(pi) {
  const configuredPort = normalizePort(process.env.PI_UI_PORT);
  const name = process.env.PI_UI_SERVICE_NAME || serviceName(process.env.TREER_AGENT_ID);
  const clients = new Set();
  const runtime = {
    activeTools: new Map(),
    busy: false,
    error: null,
    forking: false,
    lastFork: null,
    liveMessage: null,
    port: null,
  };
  let context = null;
  let server = null;
  let heartbeat = null;

  const snapshot = () => snapshotFromContext(context, runtime);
  const broadcast = () => {
    const payload = `event: snapshot\ndata: ${json(snapshot())}\n\n`;
    for (const client of clients) client.write(payload);
  };

  const startServer = async () => {
    if (server) return;
    server = createServer(async (request, response) => {
      try {
        const url = new URL(request.url ?? "/", "http://127.0.0.1");
        if ((request.method === "GET" || request.method === "HEAD") && url.pathname === "/health") {
          return sendJson(response, 200, { service: "treer-pi-ui", status: "ok" }, request.method === "HEAD");
        }
        if (request.method === "GET" && url.pathname === "/api/snapshot") {
          return sendJson(response, 200, snapshot());
        }
        if (request.method === "GET" && url.pathname === "/api/events") {
          response.writeHead(200, {
            "cache-control": "no-cache, no-transform",
            connection: "keep-alive",
            "content-type": "text/event-stream; charset=utf-8",
            "x-accel-buffering": "no",
          });
          clients.add(response);
          response.write(`event: snapshot\ndata: ${json(snapshot())}\n\n`);
          request.on("close", () => clients.delete(response));
          return;
        }
        if (request.method === "POST" && url.pathname === "/api/prompt") {
          if (!context) return sendJson(response, 503, { error: "Pi session is not ready" });
          const body = await readBody(request);
          const message = typeof body.message === "string" ? body.message.trim() : "";
          if (!message) return sendJson(response, 400, { error: "message is required" });
          const options = promptOptions(context.isIdle(), body.mode);
          pi.sendUserMessage(message, {
            ...options,
            expandPromptTemplates: true,
          });
          return sendJson(response, 202, { accepted: true });
        }
        if (request.method === "POST" && url.pathname === "/api/abort") {
          if (!context) return sendJson(response, 503, { error: "Pi session is not ready" });
          context.abort();
          return sendJson(response, 202, { accepted: true });
        }
        if (request.method === "POST" && url.pathname === "/api/compact") {
          if (!context) return sendJson(response, 503, { error: "Pi session is not ready" });
          context.compact();
          return sendJson(response, 202, { accepted: true });
        }
        if (request.method === "POST" && url.pathname === "/api/fork") {
          if (!context) return sendJson(response, 503, { error: "Pi session is not ready" });
          if (runtime.forking) return sendJson(response, 409, { error: "An Agent fork is already in progress" });
          if (!context.isIdle()) return sendJson(response, 409, { error: "Wait for the current Pi response to finish before forking" });
          if (!canForkSession(context)) {
            return sendJson(response, 409, { error: "Fork requires at least one saved conversation message" });
          }
          runtime.forking = true;
          runtime.error = null;
          broadcast();
          try {
            const child = await createForkedAgent(context);
            runtime.lastFork = {
              agentId: child.agent_id,
              name: child.name,
            };
            return sendJson(response, 201, { agent: child });
          } catch (error) {
            const detail = error?.stderr?.trim?.() || (error instanceof Error ? error.message : String(error));
            runtime.error = `Agent fork failed: ${detail}`;
            return sendJson(response, 500, { error: runtime.error });
          } finally {
            runtime.forking = false;
            broadcast();
          }
        }
        if ((request.method === "GET" || request.method === "HEAD")
          && await serveAsset(url.pathname, response, request.method === "HEAD")) return;
        sendJson(response, 404, { error: "not found" });
      } catch (error) {
        sendJson(response, 400, { error: error instanceof Error ? error.message : String(error) });
      }
    });
    server.on("clientError", (_error, socket) => socket.end("HTTP/1.1 400 Bad Request\r\n\r\n"));
    await new Promise((resolve, reject) => {
      server.once("error", reject);
      server.listen(configuredPort, "127.0.0.1", resolve);
    });
    const address = server.address();
    runtime.port = typeof address === "object" && address ? address.port : configuredPort;
    heartbeat = setInterval(() => {
      for (const client of clients) client.write(": keepalive\n\n");
    }, 15000);
  };

  const updateContext = (next) => {
    context = next;
    runtime.error = null;
  };

  pi.on("session_start", async (_event, ctx) => {
    updateContext(ctx);
    await startServer();
    if (process.env.TREER_AGENT_ID && process.env.PI_UI_AUTO_REGISTER !== "0") {
      try {
        await registerTreerUi(runtime.port, name);
        ctx.ui.setStatus("pi-ui", `UI :${runtime.port}`);
      } catch (error) {
        runtime.error = `Treer UI registration failed: ${error instanceof Error ? error.message : String(error)}`;
        ctx.ui.notify(runtime.error, "error");
      }
    }
    broadcast();
  });

  pi.on("session_info_changed", (_event, ctx) => {
    updateContext(ctx);
    broadcast();
  });
  pi.on("agent_start", (_event, ctx) => {
    updateContext(ctx);
    runtime.busy = true;
    runtime.liveMessage = null;
    runtime.activeTools.clear();
    broadcast();
  });
  pi.on("message_update", (event, ctx) => {
    updateContext(ctx);
    runtime.liveMessage = event.message ?? runtime.liveMessage;
    broadcast();
  });
  pi.on("message_end", (_event, ctx) => {
    updateContext(ctx);
    runtime.liveMessage = null;
    broadcast();
  });
  pi.on("tool_execution_start", (event, ctx) => {
    updateContext(ctx);
    runtime.activeTools.set(event.toolCallId, {
      id: event.toolCallId,
      name: event.toolName,
      args: event.args,
      result: null,
      status: "running",
    });
    broadcast();
  });
  pi.on("tool_execution_update", (event, ctx) => {
    updateContext(ctx);
    const tool = runtime.activeTools.get(event.toolCallId);
    if (tool) {
      tool.result = event.partialResult;
      broadcast();
    }
  });
  pi.on("tool_execution_end", (event, ctx) => {
    updateContext(ctx);
    runtime.activeTools.set(event.toolCallId, {
      id: event.toolCallId,
      name: event.toolName,
      args: event.args,
      result: event.result,
      status: event.isError ? "error" : "success",
    });
    broadcast();
  });
  pi.on("agent_end", (_event, ctx) => {
    updateContext(ctx);
    runtime.liveMessage = null;
    broadcast();
  });
  pi.on("agent_settled", (_event, ctx) => {
    updateContext(ctx);
    runtime.busy = false;
    runtime.liveMessage = null;
    runtime.activeTools.clear();
    broadcast();
  });
  pi.on("extension_error", (event, ctx) => {
    updateContext(ctx);
    runtime.error = event.error?.message ?? String(event.error ?? "Pi extension error");
    broadcast();
  });
  pi.on("session_shutdown", async () => {
    if (heartbeat) clearInterval(heartbeat);
    heartbeat = null;
    for (const client of clients) client.end();
    clients.clear();
    if (server) await new Promise((resolve) => server.close(resolve));
    server = null;
    runtime.port = null;
  });
}
