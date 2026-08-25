const CAPABILITIES = ["prompt.submit", "transcript.read", "state.observe", "abort"];

function itemText(item) {
  if (typeof item?.text === "string") return item.text;
  if (Array.isArray(item?.content)) {
    return item.content
      .filter((part) => part?.type === "text" && typeof part.text === "string")
      .map((part) => part.text)
      .join("");
  }
  return item?.content ?? item;
}

export function createCodexBackend(rpc, options = {}) {
  const cwd = options.cwd ?? process.cwd();
  const model = options.model;
  let threadId = options.threadId ?? null;
  let turnId = null;
  let busy = false;
  let error = null;
  let turns = [];

  rpc.events.on("turn/started", (params) => {
    turnId = params?.turn?.id ?? params?.turnId ?? turnId;
    busy = true;
  });
  rpc.events.on("turn/completed", (params) => {
    busy = false;
    turnId = null;
    const status = params?.turn?.status;
    if (status === "failed") {
      error = params?.turn?.error?.message ?? "Codex turn failed";
    }
  });
  rpc.events.on("server-error", (params) => {
    const detail = params?.error?.message ?? params?.message;
    if (params?.willRetry) {
      busy = true;
      return;
    }
    if (detail) error = String(detail);
  });

  async function ensureThread() {
    if (threadId) return threadId;
    const params = { cwd, approvalPolicy: "never", sandbox: "workspace-write" };
    if (model) params.model = model;
    const result = await rpc.request("thread/start", params);
    threadId = result?.thread?.id ?? result?.id;
    if (!threadId) throw new Error("Codex app-server did not return a thread id");
    return threadId;
  }

  return {
    capabilities: CAPABILITIES,
    async start() {
      await rpc.request("initialize", {
        clientInfo: {
          name: "treer_ais",
          title: "Treer AIS",
          version: "0.1.0",
        },
      });
      rpc.notify("initialized");
      await ensureThread();
    },
    threadId: () => threadId,
    async status() {
      if (threadId) {
        try {
          const result = await rpc.request("thread/read", { threadId, includeTurns: true });
          turns = result?.thread?.turns ?? result?.turns ?? turns;
          const last = turns.at(-1);
          const turnStatus = last?.status;
          if (turnStatus === "inProgress" || turnStatus === "in_progress") busy = true;
          else if (turnStatus === "completed" || turnStatus === "interrupted" || turnStatus === "failed") {
            busy = false;
            turnId = null;
            if (turnStatus === "failed") {
              error = last?.error?.message ?? error;
            }
          }
        } catch {
          // Keep last known busy flag if thread/read is unavailable mid-turn.
        }
      }
      return {
        ready: Boolean(threadId),
        busy,
        error,
        status: error ? "blocked" : busy ? "working" : threadId ? "idle" : "starting",
      };
    },
    async entries() {
      const id = await ensureThread();
      let result;
      try {
        result = await rpc.request("thread/read", { threadId: id, includeTurns: true });
      } catch (err) {
        const detail = err instanceof Error ? err.message : String(err);
        if (detail.includes("not materialized") || detail.includes("includeTurns")) {
          return [];
        }
        throw err;
      }
      turns = result?.thread?.turns ?? result?.turns ?? turns;
      return turns.flatMap((turn, turnIndex) => (
        turn.items ?? []
      ).map((item, itemIndex) => ({
        id: String(item.id ?? `${turn.id ?? turnIndex}:${itemIndex}`),
        kind: item.type === "userMessage" || item.type === "agentMessage" ? "message" : String(item.type ?? "item"),
        type: item.type === "userMessage" || item.type === "agentMessage" ? "message" : String(item.type ?? "item"),
        role: item.type === "userMessage" ? "user" : item.type === "agentMessage" ? "assistant" : null,
        content: itemText(item),
        created_at: null,
      })));
    },
    async prompt(text) {
      const id = await ensureThread();
      busy = true;
      error = null;
      const result = await rpc.request("turn/start", {
        threadId: id,
        input: [{ type: "text", text }],
        ...(model ? { model } : {}),
      });
      turnId = result?.turn?.id ?? turnId;
    },
    async abort() {
      if (!threadId || !turnId) return;
      await rpc.request("turn/interrupt", { threadId, turnId });
    },
  };
}
