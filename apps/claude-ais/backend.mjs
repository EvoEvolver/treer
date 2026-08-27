const CAPABILITIES = ["prompt.submit", "transcript.read", "state.observe", "abort"];

function textFromContent(content) {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return content;
  return content
    .filter((part) => part?.type === "text" && typeof part.text === "string")
    .map((part) => part.text)
    .join("");
}

export function createClaudeBackend(io, options = {}) {
  const entries = [];
  let sessionId = options.sessionId ?? null;
  let busy = false;
  let error = null;
  let ready = Boolean(sessionId);
  io.onEvent?.((event) => {
    if (!event || typeof event !== "object") return;
    if (event.type === "system" && event.subtype === "init") {
      sessionId = event.session_id ?? sessionId;
      ready = true;
      return;
    }
    if (event.type === "user" || event.type === "assistant") {
      ready = true;
      const message = event.message ?? event;
      const content = textFromContent(message.content) || message;
      const role = event.type === "user" ? "user" : "assistant";
      const last = entries.at(-1);
      if (role === "user" && last?.role === "user" && last.content === content) return;
      entries.push({
        id: String(event.uuid ?? event.id ?? `${sessionId ?? "claude"}:${entries.length}`),
        kind: "message",
        type: "message",
        role,
        content,
        created_at: null,
      });
      return;
    }
    if (event.type === "result") {
      ready = true;
      busy = false;
      if (event.is_error) error = event.result ?? "Claude turn failed";
    }
  });

  return {
    capabilities: CAPABILITIES,
    sessionId: () => sessionId,
    async start() {
      const deadline = Date.now() + 20000;
      while (!ready && Date.now() < deadline) {
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
      return sessionId;
    },
    async status() {
      return {
        ready: true,
        busy,
        error,
        status: error ? "blocked" : busy ? "working" : "idle",
      };
    },
    async entries() {
      return entries;
    },
    async prompt(text) {
      busy = true;
      error = null;
      entries.push({
        id: `user:${entries.length}`,
        kind: "message",
        type: "message",
        role: "user",
        content: text,
        created_at: null,
      });
      await io.send({
        type: "user",
        message: {
          role: "user",
          content: [{ type: "text", text }],
        },
      });
    },
    async abort() {
      await io.send({
        type: "control_request",
        request_id: `abort_${Date.now()}`,
        request: { subtype: "interrupt", ...(sessionId ? { session_id: sessionId } : {}) },
      });
      busy = false;
    },
  };
}
