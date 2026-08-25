const CAPABILITIES = ["prompt.submit", "transcript.read", "state.observe", "abort"];

export function createOpenCodeBackend(http, options = {}) {
  const baseUrl = options.baseUrl;
  const providerID = options.providerID ?? options.provider;
  const modelID = options.modelID ?? options.model;
  let sessionId = options.sessionId ?? null;
  let busy = false;
  let error = null;
  let expectedAssistants = 0;

  async function request(method, path, body) {
    const response = await http(path, {
      method,
      headers: body ? { "content-type": "application/json" } : undefined,
      body: body ? JSON.stringify(body) : undefined,
    });
    if (!response.ok) {
      const detail = await response.text();
      throw new Error(`OpenCode ${method} ${path} failed: ${response.status} ${detail}`);
    }
    if (response.status === 204) return null;
    return response.json();
  }

  async function ensureSession() {
    if (sessionId) return sessionId;
    const created = await request("POST", "/session", { title: options.title ?? "treer-agent" });
    sessionId = created?.id ?? created?.session?.id;
    if (!sessionId) throw new Error("OpenCode did not return a session id");
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
          const statuses = await request("GET", "/session/status");
          const current = statuses?.[sessionId];
          const type = current?.type ?? current?.status;
          if (type === "busy" || type === "working" || current === "busy") {
            busy = true;
          } else {
            const entries = await this.entries();
            const assistants = entries.filter((entry) => entry.role === "assistant").length;
            busy = expectedAssistants > 0 && assistants < expectedAssistants;
          }
        } catch {
          // Keep last known busy flag.
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
      const messages = await request("GET", `/session/${encodeURIComponent(id)}/message`);
      const list = Array.isArray(messages) ? messages : messages?.messages ?? [];
      return list.map((row, index) => {
        const info = row.info ?? row;
        const parts = row.parts ?? [];
        const text = parts
          .filter((part) => part.type === "text" && typeof part.text === "string")
          .map((part) => part.text)
          .join("");
        return {
          id: String(info.id ?? `${id}:${index}`),
          kind: "message",
          type: "message",
          role: info.role ?? null,
          content: text || row,
          created_at: info.time?.created ? new Date(info.time.created).toISOString() : null,
        };
      });
    },
    async prompt(text) {
      const id = await ensureSession();
      const current = await this.entries();
      expectedAssistants = current.filter((entry) => entry.role === "assistant").length + 1;
      busy = true;
      error = null;
      const body = { parts: [{ type: "text", text }] };
      if (providerID) body.providerID = providerID;
      if (modelID) body.modelID = modelID;
      await request("POST", `/session/${encodeURIComponent(id)}/prompt_async`, body);
    },
    async abort() {
      if (!sessionId) return;
      await request("POST", `/session/${encodeURIComponent(sessionId)}/abort`);
      busy = false;
    },
  };
}
