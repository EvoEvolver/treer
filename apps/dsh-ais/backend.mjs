import { randomUUID } from "node:crypto";

const CAPABILITIES = ["prompt.submit", "transcript.read", "state.observe", "abort"];

function eventRole(event) {
  if (event?.type === "user/message") return "user";
  if (event?.type === "assistant/message") return "assistant";
  return null;
}

function eventText(event) {
  const data = event?.data ?? event;
  if (typeof data === "string") return data;
  const content = data?.content ?? data?.message?.content ?? data?.text;
  if (typeof content === "string") return content;
  if (Array.isArray(content)) {
    return content
      .filter((part) => part?.type === "text" && typeof part.text === "string")
      .map((part) => part.text)
      .join("");
  }
  return "";
}

function isSurfaceEvent(event) {
  if (event?.type === "assistant/message") return true;
  if (event?.type !== "user/message") return false;
  const text = eventText(event);
  return !text.includes("<system-reminder>") && !text.startsWith("Current runtime context");
}

function historyEvents(history) {
  return (history?.events ?? []).map((entry) => entry.event ?? entry);
}

function isBusyFromEvents(events) {
  let turn = 0;
  for (const event of events) {
    if (event?.type === "turn/start") turn += 1;
    if (event?.type === "turn/end") turn = Math.max(0, turn - 1);
  }
  return turn > 0;
}

export function createDshHostBackend(http, options = {}) {
  let sessionId = options.sessionId ?? null;
  let busy = false;
  let error = null;

  async function rpc(method, payload = {}) {
    const rpcId = randomUUID();
    const response = await http(`/api/${method}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        type: "client-request",
        rpcId,
        method,
        payload,
      }),
    });
    if (!response.ok) {
      throw new Error(`DSH ${method} failed: ${response.status} ${await response.text()}`);
    }
    const body = await response.json();
    if (body?.result?.ok === false) {
      throw new Error(body.result.error?.message ?? `${method} failed`);
    }
    return body?.result?.value ?? body?.value ?? body;
  }

  async function ensureSession() {
    if (sessionId) return sessionId;
    const created = await rpc("session.create", {
      cwd: options.cwd ?? process.cwd(),
      sessionId: options.preferredSessionId,
    });
    sessionId = created?.sessionId ?? created?.id;
    if (!sessionId) throw new Error("DeepSeek Harness did not return a session id");
    if (options.provider && options.model) {
      await rpc("session.selectModel", {
        sessionId,
        provider: options.provider,
        model: options.model,
      }).catch(() => {});
    }
    return sessionId;
  }

  return {
    capabilities: CAPABILITIES,
    async start() {
      await ensureSession();
    },
    sessionId: () => sessionId,
    async status() {
      if (sessionId) {
        try {
          const history = await rpc("session.history", { sessionId, maxMessages: 1000 });
          busy = isBusyFromEvents(historyEvents(history));
        } catch {
          // Keep last known busy flag if history is unavailable mid-turn.
        }
      }
      return {
        ready: Boolean(sessionId),
        busy,
        error,
        status: error ? "blocked" : busy ? "working" : sessionId ? "idle" : "starting",
      };
    },
    async entries() {
      const id = await ensureSession();
      const history = await rpc("session.history", { sessionId: id, maxMessages: 1000 });
      return historyEvents(history)
        .filter(isSurfaceEvent)
        .map((event, index) => ({
          id: String(event.seq ?? `${id}:${index}`),
          kind: "message",
          type: "message",
          role: eventRole(event),
          content: eventText(event) || event.data || event,
          created_at: typeof event.time === "number" ? new Date(event.time).toISOString() : null,
        }));
    },
    async prompt(text) {
      const id = await ensureSession();
      busy = true;
      error = null;
      await rpc("session.prompt", {
        sessionId: id,
        mode: "queue",
        content: [{ type: "text", text }],
      });
    },
    async abort() {
      if (!sessionId) return;
      await rpc("session.cancel", { sessionId });
      busy = false;
    },
    markBusy(value) {
      busy = value;
    },
  };
}

export function createDshSdkBackend(rpc, options = {}) {
  const sessionId = options.sessionId ?? `treer-${randomUUID()}`;
  const events = [];
  let busy = false;
  let error = null;
  rpc.events.on("session.event", (params) => {
    if (params?.sessionId === sessionId && params.event) events.push(params.event);
  });
  rpc.events.on("session.status", (params) => {
    if (params?.sessionId === sessionId) busy = params.status === "running";
  });
  return {
    capabilities: ["prompt.submit", "transcript.read", "state.observe"],
    async start() {
      await rpc.request("initialize", {
        cwd: options.cwd ?? process.cwd(),
        provider: options.provider ?? process.env.DSH_PROVIDER ?? "openai",
        model: options.model ?? process.env.AIS_MODEL ?? process.env.MODEL ?? "gpt-5.6-luna",
      });
    },
    sessionId: () => sessionId,
    async status() {
      return {
        ready: true,
        busy,
        error,
        status: error ? "blocked" : busy ? "working" : "idle",
      };
    },
    async entries() {
      return events.filter(isSurfaceEvent).map((event, index) => ({
        id: String(event.seq ?? index),
        kind: "message",
        type: "message",
        role: eventRole(event),
        content: eventText(event) || event.data || event,
        created_at: typeof event.time === "number" ? new Date(event.time).toISOString() : null,
      }));
    },
    async prompt(text) {
      busy = true;
      await rpc.request("session/prompt", {
        sessionId,
        contentBlocks: [{ type: "text", text }],
      });
    },
  };
}
