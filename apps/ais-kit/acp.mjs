const CAPABILITIES = ["prompt.submit", "transcript.read", "state.observe", "abort"];

function contentText(content) {
  if (typeof content === "string") return content;
  if (!content || typeof content !== "object") return "";
  if (typeof content.text === "string") return content.text;
  if (Array.isArray(content)) return content.map(contentText).join("");
  return "";
}

function appendEntry(entries, role, text) {
  if (!text) return;
  const last = entries.at(-1);
  if (last?.role === role && last.content === text) return;
  if (role === "assistant" && last?.role === "assistant" && last._streaming) {
    last.content += text;
    return;
  }
  entries.push({
    id: `${role}:${entries.length}`,
    kind: "message",
    type: "message",
    role,
    content: text,
    created_at: null,
    ...(role === "assistant" ? { _streaming: true } : {}),
  });
}

function finishAssistant(entries) {
  const last = entries.at(-1);
  if (last?._streaming) delete last._streaming;
}

function permissionResult(params) {
  const options = Array.isArray(params?.options) ? params.options : [];
  const preferred = options.find((option) => option.optionId === "allow-once" || option.kind === "allow_once")
    || options.find((option) => option.optionId === "allow-always" || option.kind === "allow_always")
    || options.find((option) => String(option.optionId ?? "").includes("allow"))
    || options[0];
  return {
    outcome: {
      outcome: "selected",
      optionId: preferred?.optionId ?? "allow-once",
    },
  };
}

function publicEntries(entries) {
  return entries.map(({ _streaming, ...entry }) => entry);
}

export function selectAuthMethod(methods, preferredIds = []) {
  const list = Array.isArray(methods) ? methods : [];
  const ids = new Set(list.map((method) => method?.id).filter(Boolean));
  for (const id of preferredIds) {
    if (ids.has(id)) return id;
  }
  return list[0]?.id ?? null;
}

export function createAcpBackend(rpc, options = {}) {
  const entries = [];
  let sessionId = options.sessionId ?? null;
  let busy = false;
  let error = null;
  let ready = Boolean(sessionId);

  function handleUpdate(params) {
    const update = params?.update ?? params;
    const kind = update?.sessionUpdate;
    if (params?.sessionId && sessionId && params.sessionId !== sessionId) return;
    if (kind === "agent_message_chunk") {
      appendEntry(entries, "assistant", contentText(update.content));
      return;
    }
    if (kind === "user_message_chunk") {
      appendEntry(entries, "user", contentText(update.content));
    }
  }

  function handleRequest(method, params, id) {
    if (method === "session/request_permission") {
      rpc.respond(id, permissionResult(params));
      return;
    }
    if (method === "cursor/ask_question") {
      rpc.respond(id, { outcome: { outcome: "skipped", reason: "unattended AIS adapter" } });
      return;
    }
    if (method === "cursor/create_plan") {
      rpc.respond(id, { outcome: { outcome: "accepted" } });
      return;
    }
    rpc.respond(id, { outcome: { outcome: "cancelled" } });
  }

  rpc.events.on("session/update", handleUpdate);
  rpc.events.on("request", handleRequest);

  return {
    capabilities: CAPABILITIES,
    sessionId: () => sessionId,
    async start() {
      const init = await rpc.request("initialize", {
        protocolVersion: options.protocolVersion ?? 1,
        clientCapabilities: options.clientCapabilities ?? {
          fs: { readTextFile: false, writeTextFile: false },
          terminal: false,
        },
        clientInfo: options.clientInfo ?? { name: "treer-ais", version: "0.1.0" },
      });
      const methodId = options.selectAuth
        ? options.selectAuth(init?.authMethods ?? [])
        : selectAuthMethod(init?.authMethods ?? [], options.preferredAuthIds ?? []);
      if (methodId) {
        await rpc.request("authenticate", {
          methodId,
          ...(options.authParams ?? {}),
        });
      }
      const created = await rpc.request("session/new", {
        cwd: options.cwd ?? process.cwd(),
        mcpServers: options.mcpServers ?? [],
      });
      sessionId = created?.sessionId ?? created?.session_id ?? sessionId;
      if (!sessionId) throw new Error("ACP session/new did not return sessionId");
      ready = true;
      return sessionId;
    },
    async status() {
      return {
        ready,
        busy,
        error,
        status: error ? "blocked" : busy ? "working" : ready ? "idle" : "starting",
      };
    },
    async entries() {
      return publicEntries(entries);
    },
    async prompt(text) {
      if (!sessionId) throw new Error("ACP session is not ready");
      busy = true;
      error = null;
      appendEntry(entries, "user", text);
      try {
        await rpc.request("session/prompt", {
          sessionId,
          prompt: [{ type: "text", text }],
        });
        finishAssistant(entries);
      } catch (err) {
        error = err instanceof Error ? err.message : String(err);
        throw err;
      } finally {
        busy = false;
      }
    },
    async abort() {
      if (!sessionId) return;
      try {
        await rpc.request("session/cancel", { sessionId });
      } catch {
        rpc.notify("session/cancel", { sessionId });
      }
      busy = false;
    },
  };
}
