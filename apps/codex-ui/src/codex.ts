import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { EventEmitter } from "node:events";

import { JsonRpcClient } from "./jsonrpc.js";
import {
  fallbackModelOption,
  mapItem,
  mapModelOption,
  mapReasoningEffort,
  mapTurn,
  yoloResponse,
  type CodexItem,
  type CodexTurn,
  type ModelOption,
  type ReasoningEffort,
  type TurnDto,
} from "./map.js";

const HISTORY_PAGE_SIZE = 50;

interface LiveItem {
  item: CodexItem;
  sequence: number;
  turnId: string;
}

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
  hasOlderItems: boolean;
}

export class CodexRuntime extends EventEmitter {
  private child: ChildProcessWithoutNullStreams | null = null;
  private client: JsonRpcClient | null = null;
  private ready = false;
  private fullTurns: TurnDto[] = [];
  private liveItems = new Map<string, LiveItem>();
  private liveSequence = 0;
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
        sandbox: "danger-full-access",
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
      hasOlderItems: false,
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
    const input = [{ type: "text", text, text_elements: [] }];
    if (this.thread.status === "running" && this.thread.activeTurnId) {
      await this.client.request<{ turnId: string }>(
        "turn/steer",
        {
          threadId: this.thread.id,
          expectedTurnId: this.thread.activeTurnId,
          input,
        },
        60_000,
      );
      this.thread.updatedAt = new Date().toISOString();
      this.emit("state");
      return;
    }
    const turn = await this.client.request<{ turn: { id: string } }>(
      "turn/start",
      {
        threadId: this.thread.id,
        input,
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

  snapshot(historyLimit?: number) {
    const thread = this.thread && historyLimit !== undefined
      ? this.threadWithHistoryLimit(historyLimit)
      : this.thread;
    return {
      ready: this.ready,
      thread,
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

  private threadWithHistoryLimit(historyLimit: number) {
    const thread = this.thread!;
    const sourceTurns = this.mergedTurns();
    const totalItems = sourceTurns.reduce((count, turn) => count + turn.items.length, 0);
    let remaining = Math.min(Math.max(1, historyLimit), totalItems);
    const visibleTurns: TurnDto[] = [];
    for (let index = sourceTurns.length - 1; index >= 0 && remaining > 0; index -= 1) {
      const turn = sourceTurns[index];
      const items = turn.items.slice(-remaining);
      remaining -= items.length;
      visibleTurns.unshift({ ...turn, items });
    }
    return {
      ...thread,
      turns: visibleTurns,
      hasOlderItems: totalItems > historyLimit,
    };
  }

  private mergedTurns() {
    const turns = this.fullTurns.map((turn) => ({ ...turn, items: [...turn.items] }));
    const byId = new Map(turns.map((turn) => [turn.id, turn]));
    const liveByTurn = new Map<string, LiveItem[]>();
    for (const entry of this.liveItems.values()) {
      const items = liveByTurn.get(entry.turnId) ?? [];
      items.push(entry);
      liveByTurn.set(entry.turnId, items);
    }
    for (const [turnId, entries] of liveByTurn) {
      entries.sort((left, right) => left.sequence - right.sequence);
      let turn = byId.get(turnId);
      if (!turn) {
        turn = {
          id: turnId,
          startedAt: null,
          status: turnId === this.thread?.activeTurnId ? "inProgress" : "completed",
          error: null,
          items: [],
        };
        turns.push(turn);
        byId.set(turnId, turn);
      }
      // The persisted thread omits tool activity and may also omit item IDs. For a
      // turn observed by this process, the live stream is therefore both richer
      // and the only reliable source of ordering.
      turn.items = entries.map((entry, index) => mapItem(entry.item, turnId, index));
    }
    return turns;
  }

  private upsertLiveItem(turnId: string, item: CodexItem) {
    if (typeof item.id !== "string") return;
    const current = this.liveItems.get(item.id);
    this.liveItems.set(item.id, {
      item,
      sequence: current?.sequence ?? this.liveSequence++,
      turnId,
    });
  }

  private updateLiveItem(itemId: string, update: (item: CodexItem) => CodexItem) {
    const current = this.liveItems.get(itemId);
    if (!current) return false;
    this.liveItems.set(itemId, { ...current, item: update(current.item) });
    return true;
  }

  private publishLiveHistory() {
    if (!this.thread) return;
    this.thread.turns = this.mergedTurns();
    this.emit("state");
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
      const statusType = record.status && typeof record.status === "object" ? record.status.type : null;
      this.fullTurns = Array.isArray(record.turns) ? record.turns.map((turn) => mapTurn(turn)) : [];
      const active = this.fullTurns.find((turn) => turn.status === "inProgress");
      const running = statusType === "active" || active !== undefined;
      const activeTurnId = running
        ? active?.id ?? this.thread.activeTurnId
        : null;
      this.thread = {
        ...this.thread,
        id: record.id ?? this.thread.id,
        title: record.name?.trim() || this.thread.title,
        cwd: record.cwd || this.thread.cwd,
        model: this.thread.model,
        reasoningEffort: this.thread.reasoningEffort,
        status: running ? "running" : statusType === "systemError" ? "error" : "idle",
        activeTurnId,
        lastError: this.fullTurns.find((turn) => turn.error)?.error ?? null,
        updatedAt: new Date().toISOString(),
        turns: [],
        hasOlderItems: false,
      };
      this.thread.turns = this.mergedTurns();
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
    const params = event.params ?? {};
    if ((method === "item/started" || method === "item/completed")
        && typeof params.turnId === "string" && params.item && typeof params.item === "object") {
      this.upsertLiveItem(params.turnId, params.item as CodexItem);
      this.publishLiveHistory();
      return;
    }
    if (method === "item/commandExecution/outputDelta" && typeof params.itemId === "string") {
      this.updateLiveItem(params.itemId, (item) => ({
        ...item,
        aggregatedOutput: `${item.aggregatedOutput ?? ""}${typeof params.delta === "string" ? params.delta : ""}`,
      }));
      this.publishLiveHistory();
      return;
    }
    if ((method === "item/agentMessage/delta" || method === "item/reasoning/textDelta")
        && typeof params.itemId === "string") {
      this.updateLiveItem(params.itemId, (item) => ({
        ...item,
        text: `${item.text ?? ""}${typeof params.delta === "string" ? params.delta : ""}`,
      }));
      this.publishLiveHistory();
      return;
    }
    if (method === "turn/started" && params.turn && typeof params.turn === "object") {
      const turn = params.turn as CodexTurn;
      const turnId = typeof turn.id === "string" ? turn.id : null;
      if (turnId && this.thread) {
        for (const item of turn.items ?? []) this.upsertLiveItem(turnId, item);
        this.thread.status = "running";
        this.thread.activeTurnId = turnId;
        this.publishLiveHistory();
      }
      return;
    }
    if (method === "turn/completed" && params.turn && typeof params.turn === "object") {
      const turn = params.turn as CodexTurn;
      const turnId = typeof turn.id === "string" ? turn.id : null;
      if (turnId && this.thread) {
        for (const item of turn.items ?? []) this.upsertLiveItem(turnId, item);
        this.thread.status = turn.status === "failed" ? "error" : "idle";
        this.thread.activeTurnId = null;
        this.publishLiveHistory();
      }
      return;
    }
    if (method.startsWith("thread/") || method === "error") {
      try {
        await this.refresh();
      } catch (error) {
        this.emit("log", `refresh failed: ${error}`);
      }
    }
  }
}
