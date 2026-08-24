import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { EventEmitter } from "node:events";

import { JsonRpcClient } from "./jsonrpc.js";
import {
  fallbackModelOption,
  mapModelOption,
  mapReasoningEffort,
  mapTurn,
  yoloResponse,
  type ModelOption,
  type ReasoningEffort,
  type TurnDto,
} from "./map.js";

export interface ThreadState {
  id: string;
  title: string;
  cwd: string;
  model: string | null;
  reasoningEffort: ReasoningEffort | null;
  status: "idle" | "running" | "error";
  activeTurnId: string | null;
  lastError: string | null;
  createdAt: string;
  updatedAt: string;
  turns: TurnDto[];
}

export class CodexRuntime extends EventEmitter {
  private child: ChildProcessWithoutNullStreams | null = null;
  private client: JsonRpcClient | null = null;
  private ready = false;
  thread: ThreadState | null = null;
  models: ModelOption[] = [];

  constructor(
    private readonly command: string,
    private readonly cwd: string,
  ) {
    super();
  }

  async start() {
    const child = spawn(this.command, ["app-server", "--listen", "stdio://"], {
      stdio: ["pipe", "pipe", "pipe"],
      cwd: this.cwd,
    });
    this.child = child;
    const client = new JsonRpcClient(child.stdout, child.stdin);
    this.client = client;

    child.stderr.on("data", (chunk) => {
      const text = chunk.toString().trim();
      if (text) {
        this.emit("log", text);
      }
    });
    child.on("exit", (code, signal) => {
      this.ready = false;
      this.emit("exit", { code, signal });
    });

    client.on("notification", (event) => {
      void this.onNotification(event as { method?: string; params?: Record<string, unknown> });
    });
    client.on("request", (request) => {
      const method = String((request as { method?: string }).method ?? "");
      const id = (request as { id: number }).id;
      const params = (request as { params?: unknown }).params;
      try {
        client.respond(id, yoloResponse(method, params));
      } catch (error) {
        this.emit("log", `failed to auto-approve ${method}: ${error}`);
      }
    });

    await client.request("initialize", {
      clientInfo: {
        name: "codex-agent-ui",
        title: "Codex Agent UI",
        version: "0.1.0",
      },
      capabilities: { experimentalApi: true },
    });
    this.ready = true;
    this.models = await this.loadModels().catch((error) => {
      this.emit("log", `model/list failed: ${error}`);
      return [] as ModelOption[];
    });

    const started = await client.request<{
      thread: { id: string; name?: string | null; cwd?: string };
      model?: string;
      reasoningEffort?: string | null;
    }>(
      "thread/start",
      {
        cwd: this.cwd,
        approvalPolicy: "never",
        sandbox: "workspace-write",
        experimentalRawEvents: false,
        persistExtendedHistory: true,
      },
      60_000,
    );
    const now = new Date().toISOString();
    const model = started.model ?? (started.thread as { model?: string }).model ?? null;
    const reasoningEffort = mapReasoningEffort(
      started.reasoningEffort ?? (started as { reasoning_effort?: unknown }).reasoning_effort,
    );
    this.thread = {
      id: started.thread.id,
      title: started.thread.name?.trim() || "Codex",
      cwd: started.thread.cwd || this.cwd,
      model,
      reasoningEffort,
      status: "idle",
      activeTurnId: null,
      lastError: null,
      createdAt: now,
      updatedAt: now,
      turns: [],
    };
    if (this.models.length === 0) {
      this.models = fallbackModelOption(model, reasoningEffort);
    }
    this.applyModelDefaults();
    await this.refresh().catch((error) => this.emit("log", `initial refresh skipped: ${error}`));
    this.emit("state");
  }

  async prompt(text: string) {
    if (!this.client || !this.thread) {
      throw new Error("Codex is not ready");
    }
    const turn = await this.client.request<{ turn: { id: string } }>(
      "turn/start",
      {
        threadId: this.thread.id,
        input: [{ type: "text", text, text_elements: [] }],
        ...(this.thread.model ? { model: this.thread.model } : {}),
        ...(this.thread.reasoningEffort ? { effort: this.thread.reasoningEffort } : {}),
      },
      60_000,
    );
    this.thread.status = "running";
    this.thread.activeTurnId = turn.turn.id;
    this.thread.updatedAt = new Date().toISOString();
    this.emit("state");
    await this.refresh().catch((error) => {
      this.emit("log", `post-prompt refresh deferred: ${error}`);
    });
  }

  async interrupt() {
    if (!this.client || !this.thread?.activeTurnId) {
      return;
    }
    await this.client.request("turn/interrupt", {
      threadId: this.thread.id,
      turnId: this.thread.activeTurnId,
    });
    await this.refresh();
  }

  async updateSettings(input: { model?: string; reasoningEffort?: string | null }) {
    if (!this.thread) {
      throw new Error("Codex is not ready");
    }
    const nextModel = typeof input.model === "string" && input.model.trim() ? input.model.trim() : this.thread.model;
    const option = this.models.find((entry) => entry.model === nextModel) ?? null;
    let nextEffort = input.reasoningEffort === undefined
      ? this.thread.reasoningEffort
      : mapReasoningEffort(input.reasoningEffort);
    if (option) {
      const supported = option.supportedReasoningEfforts.map((entry) => entry.reasoningEffort);
      if (nextEffort && supported.length > 0 && !supported.includes(nextEffort)) {
        nextEffort = option.defaultReasoningEffort;
      }
      if (!nextEffort) {
        nextEffort = option.defaultReasoningEffort;
      }
    }
    this.thread = {
      ...this.thread,
      model: nextModel,
      reasoningEffort: nextEffort,
      updatedAt: new Date().toISOString(),
    };
    this.emit("state");
  }

  async stop() {
    this.client?.close();
    this.child?.kill("SIGTERM");
    this.client = null;
    this.child = null;
    this.ready = false;
  }

  snapshot() {
    return {
      ready: this.ready,
      thread: this.thread,
      models: this.models,
    };
  }

  private async loadModels() {
    if (!this.client) {
      return [] as ModelOption[];
    }
    const models: ModelOption[] = [];
    let cursor: string | undefined;
    do {
      const response = await this.client.request<{
        data?: unknown[];
        nextCursor?: string | null;
        next_cursor?: string | null;
      }>("model/list", {
        limit: 100,
        includeHidden: false,
        ...(cursor ? { cursor } : {}),
      });
      const batch = Array.isArray(response.data) ? response.data : [];
      for (const [index, entry] of batch.entries()) {
        const mapped = mapModelOption(entry, models.length + index);
        if (mapped && !mapped.hidden) {
          models.push(mapped);
        }
      }
      cursor = response.nextCursor ?? response.next_cursor ?? undefined;
    } while (cursor);
    return models;
  }

  private applyModelDefaults() {
    if (!this.thread) {
      return;
    }
    const option =
      this.models.find((entry) => entry.model === this.thread?.model) ??
      this.models.find((entry) => entry.isDefault) ??
      this.models[0] ??
      null;
    if (!option) {
      return;
    }
    const supported = option.supportedReasoningEfforts.map((entry) => entry.reasoningEffort);
    const reasoningEffort =
      this.thread.reasoningEffort && supported.includes(this.thread.reasoningEffort)
        ? this.thread.reasoningEffort
        : option.defaultReasoningEffort;
    this.thread = {
      ...this.thread,
      model: this.thread.model ?? option.model,
      reasoningEffort,
    };
  }

  private async refresh() {
    if (!this.client || !this.thread) {
      return;
    }
    try {
      const response = await this.client.request<{ thread: Record<string, unknown> }>("thread/read", {
        threadId: this.thread.id,
        includeTurns: true,
      });
      const record = response.thread as {
        id?: string;
        name?: string | null;
        cwd?: string;
        status?: { type?: string; activeFlags?: string[] };
        turns?: Array<Record<string, unknown>>;
        preview?: string;
      };
      const turns = Array.isArray(record.turns) ? record.turns.map((turn) => mapTurn(turn)) : [];
      const active = turns.find((turn) => turn.status === "inProgress");
      const statusType = record.status && typeof record.status === "object" ? record.status.type : null;
      this.thread = {
        ...this.thread,
        id: record.id ?? this.thread.id,
        title: record.name?.trim() || this.thread.title,
        cwd: record.cwd || this.thread.cwd,
        model: this.thread.model,
        reasoningEffort: this.thread.reasoningEffort,
        status: statusType === "active" || active ? "running" : statusType === "systemError" ? "error" : "idle",
        activeTurnId: active?.id ?? null,
        lastError: turns.find((turn) => turn.error)?.error ?? null,
        updatedAt: new Date().toISOString(),
        turns,
      };
      this.emit("state");
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (message.includes("not materialized")) {
        this.emit("state");
        return;
      }
      throw error;
    }
  }

  private async onNotification(event: { method?: string; params?: Record<string, unknown> }) {
    const method = event.method ?? "";
    if (
      method.startsWith("turn/") ||
      method.startsWith("item/") ||
      method.startsWith("thread/") ||
      method === "error"
    ) {
      try {
        await this.refresh();
      } catch (error) {
        this.emit("log", `refresh failed: ${error}`);
      }
    }
  }
}
