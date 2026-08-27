import { createServer } from "node:http";
import { randomBytes } from "node:crypto";
import {
  envelopeTranscriptEntry,
  parseTranscriptPageQuery,
  transcriptPageFromEntries,
} from "./transcript.mjs";
import { createOperationLog } from "./operations.mjs";

const MAX_BODY_BYTES = 1024 * 1024;
export const AIS_PROTOCOL = "treer.agent-interface/v1";
export const REQUIRED_CAPABILITIES = [
  "prompt.submit",
  "transcript.read",
  "state.observe",
];

export function newInstanceId(prefix) {
  return `${prefix}_${randomBytes(16).toString("hex")}`;
}

export function json(value) {
  return JSON.stringify(value, (_key, item) =>
    typeof item === "bigint" ? item.toString() : item,
  );
}

export async function readJsonBody(request) {
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

export function sendJson(response, status, value, headOnly = false) {
  const body = json(value);
  response.writeHead(status, {
    "cache-control": "no-store",
    "content-length": Buffer.byteLength(body),
    "content-type": "application/json; charset=utf-8",
  });
  response.end(headOnly ? undefined : body);
}

export async function listenLoopback(server, port = 0) {
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, "127.0.0.1", resolve);
  });
  const address = server.address();
  return typeof address === "object" && address ? address.port : port;
}

export function createAisServer(options) {
  const {
    instanceId,
    capabilities = REQUIRED_CAPABILITIES,
    uiPath,
    getStatus,
    getEntries,
    submitPrompt,
    abort,
    extraHandler,
  } = options;
  const operations = createOperationLog();

  const server = createServer(async (request, response) => {
    try {
      const url = new URL(request.url ?? "/", "http://127.0.0.1");
      const headOnly = request.method === "HEAD";
      if ((request.method === "GET" || headOnly) && url.pathname === "/v1/manifest") {
        return sendJson(response, 200, {
          protocol: AIS_PROTOCOL,
          instance_id: instanceId,
          capabilities,
          ...(uiPath ? { ui_path: uiPath } : {}),
        }, headOnly);
      }
      if ((request.method === "GET" || headOnly) && url.pathname === "/v1/health") {
        const status = await getStatus();
        return sendJson(response, 200, {
          instance_id: instanceId,
          status: status.ready === false ? "starting" : "ok",
        }, headOnly);
      }
      if (request.method === "GET" && url.pathname === "/v1/status") {
        const status = await getStatus();
        return sendJson(response, 200, {
          agent_id: process.env.TREER_AGENT_ID || null,
          interface_instance_id: instanceId,
          status: status.status ?? (status.error ? "blocked" : status.busy ? "working" : "idle"),
          busy: Boolean(status.busy),
          error: status.error ?? null,
        });
      }
      if (request.method === "GET" && url.pathname === "/v1/transcript") {
        const { page, limit } = parseTranscriptPageQuery(url.searchParams);
        const entries = (await getEntries()).map((entry, index) =>
          envelopeTranscriptEntry(entry, instanceId, index));
        return sendJson(response, 200, {
          agent_id: process.env.TREER_AGENT_ID || "",
          interface_instance_id: instanceId,
          ...transcriptPageFromEntries(entries, page, limit),
        });
      }
      if (request.method === "POST" && url.pathname === "/v1/prompts") {
        const body = await readJsonBody(request);
        const operationId = typeof body.operation_id === "string" ? body.operation_id.trim() : "";
        const text = typeof body.text === "string" ? body.text.trim() : "";
        if (!operationId) return sendJson(response, 400, { error: "operation_id is required" });
        if (!text) return sendJson(response, 400, { error: "text is required" });
        const claimed = operations.claim(operationId);
        if (claimed.duplicate) {
          return sendJson(response, 202, { accepted: true, duplicate: true, operation_id: operationId });
        }
        await submitPrompt({ operationId, text, mode: body.mode });
        return sendJson(response, 202, { accepted: true, operation_id: operationId });
      }
      if (request.method === "POST" && url.pathname === "/v1/abort") {
        if (!abort) return sendJson(response, 404, { error: "not found" });
        await abort();
        return sendJson(response, 202, { accepted: true });
      }
      if (extraHandler && await extraHandler(request, response, url)) return;
      sendJson(response, 404, { error: "not found" });
    } catch (error) {
      sendJson(response, 400, { error: error instanceof Error ? error.message : String(error) });
    }
  });
  server.on("clientError", (_error, socket) => socket.end("HTTP/1.1 400 Bad Request\r\n\r\n"));

  return {
    instanceId,
    capabilities,
    server,
    async listen(port = Number.parseInt(process.env.AIS_PORT ?? "0", 10) || 0) {
      const bound = await listenLoopback(server, port);
      this.port = bound;
      return bound;
    },
    async close() {
      await new Promise((resolve) => server.close(resolve));
    },
  };
}
