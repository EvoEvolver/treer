import { createReadStream, existsSync, statSync } from "node:fs";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { extname, join, normalize, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

import { WebSocketServer } from "ws";

import { CodexRuntime } from "./codex.js";

const appRoot = resolve(fileURLToPath(new URL("..", import.meta.url)));
const publicDir = resolve(process.env.CODEX_UI_PUBLIC_DIR || join(appRoot, "public"));
const port = Number(process.env.CODEX_UI_PORT || "4173");
const cwd = resolve(process.env.CODEX_UI_CWD || process.cwd());
const command = process.env.CODEX_BIN || "codex";
const agentId = process.env.TREER_AGENT_ID || "";
const interfaceInstanceId = process.env.CODEX_UI_INSTANCE_ID || `codex-ui-${process.pid}`;
const interfaceCapabilities = ["prompt.submit", "transcript.read", "state.observe", "abort"];

const runtime = new CodexRuntime(command, cwd);
const sockets = new Set<{ send: (data: string) => void }>();
const completedOperations = new Map<string, number>();

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

async function readJson(request: IncomingMessage) {
  const chunks: Buffer[] = [];
  for await (const chunk of request) {
    chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
  }
  if (!chunks.length) return {} as Record<string, unknown>;
  return JSON.parse(Buffer.concat(chunks).toString("utf8")) as Record<string, unknown>;
}

function statePayload() {
  const snapshot = runtime.snapshot();
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
    thread: snapshot.thread,
    modelOptions: snapshot.models,
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
  const encoded = JSON.stringify({ type: "state", ...statePayload() });
  for (const socket of sockets) {
    try {
      socket.send(encoded);
    } catch {
      sockets.delete(socket);
    }
  }
}

runtime.on("state", broadcast);
runtime.on("log", (message) => console.log(`[codex] ${message}`));

const server = createServer(async (request, response) => {
  const method = request.method ?? "GET";
  const url = new URL(request.url ?? "/", "http://127.0.0.1");
  const path = requestPath(url);
  try {
    if (path === "/v1/manifest" && (method === "GET" || method === "HEAD")) {
      send(response, 200, {
        protocol: "treer.agent-interface/v1",
        instance_id: interfaceInstanceId,
        capabilities: interfaceCapabilities,
        ui_path: "/",
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
        busy: payload.runtime.state === "working",
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
      if (completedOperations.has(operationId)) {
        send(response, 202, { accepted: true, duplicate: true, operation_id: operationId });
        return;
      }
      completedOperations.set(operationId, Date.now());
      try {
        await runtime.prompt(text);
      } catch (error) {
        completedOperations.delete(operationId);
        throw error;
      }
      while (completedOperations.size > 1024) {
        completedOperations.delete(completedOperations.keys().next().value!);
      }
      send(response, 202, { accepted: true, operation_id: operationId });
      return;
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
            title: payload.thread?.title ?? "Codex",
            ui: true,
            capabilities: interfaceCapabilities,
          });
      return;
    }
    if (path === "/api/state" && method === "GET") {
      send(response, 200, statePayload());
      return;
    }
    if (path === "/api/prompt" && method === "POST") {
      const body = await readJson(request);
      const prompt = typeof body.prompt === "string" ? body.prompt.trim() : "";
      if (!prompt) {
        send(response, 400, { error: "prompt is required" });
        return;
      }
      await runtime.prompt(prompt);
      send(response, 200, statePayload());
      return;
    }
    if (path === "/api/interrupt" && method === "POST") {
      await runtime.interrupt();
      send(response, 200, statePayload());
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
      send(response, 200, statePayload());
      return;
    }

    const relative = path === "/" ? "index.html" : path.slice(1);
    const file = normalize(join(publicDir, relative));
    if ((file === publicDir || file.startsWith(`${publicDir}${sep}`)) && serveFile(response, file)) return;
    if (method === "GET" && serveFile(response, join(publicDir, "index.html"))) return;
    send(response, 404, { error: "not found" });
  } catch (error) {
    console.error(error);
    send(response, 500, { error: error instanceof Error ? error.message : String(error) });
  }
});

const socketsServer = new WebSocketServer({ noServer: true });
server.on("upgrade", (request, socket, head) => {
  const path = requestPath(new URL(request.url ?? "/", "http://127.0.0.1"));
  if (path !== "/ws") {
    socket.destroy();
    return;
  }
  socketsServer.handleUpgrade(request, socket, head, (webSocket) => {
    sockets.add(webSocket);
    webSocket.send(JSON.stringify({ type: "state", ...statePayload() }));
    webSocket.on("close", () => sockets.delete(webSocket));
  });
});

server.listen(port, "127.0.0.1", () => {
  console.log(`Treer Codex UI listening on http://127.0.0.1:${port}`);
  runtime.start().catch((error) => console.error("failed to start Codex app-server", error));
});

for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.on(signal, () => {
    void runtime.stop().finally(() => process.exit(0));
  });
}
