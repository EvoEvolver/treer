import type { ChildProcessWithoutNullStreams } from "node:child_process";
import { spawnProvider, stopProcessTree } from "../../scripts/process.mjs";
import { EventEmitter } from "node:events";
import { readGoalState, writeGoalState } from "../goal-storage.js";
import { randomUUID } from "node:crypto";

import { JsonRpcClient } from "../jsonrpc.js";
import { withHistoryLimit, type HarnessImage, type HarnessInfo, type HarnessRuntime, type ThreadState } from "../runtime.js";
import { loadThreadId, saveThreadId } from "../thread-state.js";
import type { Goal, GoalUpdate } from "../model.js";
import { GoalError, mapGoal, GoalGuard } from "../goals.js";
import { SessionTransferError, type SessionBundle, type SessionImportOptions } from "../session-transfer.js";
import { exportNativeSession, importNativeSession } from "./session-files.js";
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
} from "./codex-map.js";

const HISTORY_PAGE_SIZE = 50;

interface LiveItem {
  item: CodexItem;
  sequence: number;
  turnId: string;
}

export class CodexRuntime extends EventEmitter implements HarnessRuntime {
  readonly harness: HarnessInfo = { id: "codex", name: "Codex", capabilities: { images: true, steer: true, backgroundTasks: false, goal: { pause: true, budget: true }, sessionTransfer: { export: true, import: true } } };
  private goal: Goal | null = null;
  private goalExitRecord: Goal | null = null;
  private pendingGoalExit: Goal | null = null;
  private goalMutating = false;
  private goalGuard = new GoalGuard();
  private goalBudgetExit = false;
  private goalTurnsSeen = new Set<string>();
  private child: ChildProcessWithoutNullStreams | null = null;
  private client: JsonRpcClient | null = null;
  private ready = false;
  private fullTurns: TurnDto[] = [];
  private liveItems = new Map<string, LiveItem>();
  private liveSequence = 0;
  private turnPlans = new Map<string, CodexItem>();
  thread: ThreadState | null = null;
  models: ModelOption[] = [];

  constructor(
    private readonly command: string,
    private readonly cwd: string,
    private readonly options: { threadId?: string; stateFile?: string } = {},
  ) {
    super();
  }

  async exportSession() {
    if (!this.ready || !this.thread) throw new SessionTransferError("not_ready", "No confirmed Codex session to export");
    return exportNativeSession("codex", this.thread.id, this.cwd);
  }

  async importSession(bundle: SessionBundle, options: SessionImportOptions) {
    if (bundle.harness !== "codex") throw new SessionTransferError("unsupported", "Codex cannot import another harness session");
    return importNativeSession(bundle, options);
  }

  async start() {
    const threadId = this.options.threadId || loadThreadId(this.options.stateFile);
    const child = spawnProvider(this.command, ["app-server", "--listen", "stdio://"], {
      stdio: ["pipe", "pipe", "pipe"],
      cwd: this.cwd,
    });
    this.child = child;
    const client = new JsonRpcClient(child.stdout, child.stdin);
    this.client = client;
    child.on("error", (error) => { client.close(); this.emit("log", `Codex process failed: ${error.message}`); });

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
    client.notify("initialized");
    this.models = await this.loadModels().catch((error) => {
      this.emit("log", `model/list failed: ${error}`);
      return [] as ModelOption[];
    });

    const started = await client.request<{
      thread: { id: string; name?: string | null; cwd?: string; turns?: CodexTurn[] };
      model?: string;
      reasoningEffort?: string | null;
    }>(
      threadId ? "thread/resume" : "thread/start",
      {
        ...(threadId ? { threadId } : {}),
        cwd: this.cwd,
        approvalPolicy: "never",
        sandbox: "danger-full-access",
        experimentalRawEvents: false,
        persistExtendedHistory: true,
      },
      60_000,
    ).catch((error) => {
      throw new Error(`${threadId ? `Failed to resume Codex thread ${threadId}; no new thread was created` : "Failed to start Codex thread"}: ${error instanceof Error ? error.message : String(error)}`);
    });
    if (!started.thread?.id || (threadId && started.thread.id !== threadId)) {
      throw new Error(`Codex returned an unexpected thread ID when ${threadId ? `resuming ${threadId}` : "starting"}`);
    }
    saveThreadId(this.options.stateFile, started.thread.id);
    this.fullTurns = (started.thread.turns ?? []).map(mapTurn);
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
      turns: this.fullTurns,
      hasOlderItems: false,
    };
    if (this.models.length === 0) {
      this.models = fallbackModelOption(model, reasoningEffort);
    }
    this.applyModelDefaults();
    await this.refresh().catch((error) => this.emit("log", `initial refresh skipped: ${error}`));
    try {
      this.goal = mapGoal((await client.request<{ goal: unknown }>("thread/goal/get", { threadId: this.thread.id })).goal);
      if (this.options.stateFile) {
        const saved = readGoalState(this.options.stateFile + ".goal-exit.json") as { threadId?: string; goal?: Goal; pendingExit?: Goal; guard?: { id?: string; objective?: string; roundBudget?: number | null; roundsUsed?: number } } | undefined;
        if (saved?.threadId === this.thread.id) {
          // A durable intent belongs to this thread/generation. Native absence
          // confirms release even when we crashed before saving its receipt.
          if (!this.goal) this.goal = this.goalExitRecord = saved.pendingExit ?? saved.goal ?? null;
          if (this.goal && saved.guard?.objective === this.goal.objective) { this.goal.id = saved.guard.id; this.goal.roundBudget = saved.guard.roundBudget; this.goal.roundsUsed = saved.guard.roundsUsed; }
        }
      }
      if (this.goal) { this.goal.id ??= randomUUID(); this.goal.roundBudget ??= null; this.goalGuard = new GoalGuard(undefined, this.goal.roundBudget, this.goal.tokenBudget); this.goalGuard.rounds = this.goal.roundsUsed ?? 0; }
    } catch (error) {
      if (!/method not found|unknown (?:method|variant)|not supported|unrecognized/i.test(String(error))) throw error;
      this.harness.capabilities.goal = null;
      this.emit("log", "This Codex version does not support goals");
    }
    this.ready = true;
    this.emit("state");
  }

  async prompt(text: string, images: HarnessImage[] = []) {
    if (!this.client || !this.thread) {
      throw new Error("Codex is not ready");
    }
    const command = /^\/goal(?:\s+([\s\S]*))?$/.exec(text.trim());
    if (command && !images.length) {
      if (command[1]?.trim() === "clear") await this.clearGoal();
      else if (command[1]?.trim()) await this.updateGoal({ objective: command[1].trim() });
      return;
    }
    const input = [...(text ? [{ type: "text", text, text_elements: [] }] : []), ...images.map((image) => ({ type: "localImage", path: image.path }))];
    if (this.thread.status === "running" && this.thread.activeTurnId) {
      const steer = (expectedTurnId: string) => this.client!.request<{ turnId: string }>(
        "turn/steer",
        { threadId: this.thread!.id, expectedTurnId, input },
        60_000,
      );
      try {
        await steer(this.thread.activeTurnId);
      } catch (error) {
        // Codex names the turn that is really active when our idea of it is stale.
        const actual = /found `([^`]+)`/.exec(error instanceof Error ? error.message : String(error))?.[1];
        if (!actual || actual === this.thread.activeTurnId) throw error;
        this.thread.activeTurnId = actual;
        await steer(actual);
      }
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
    stopProcessTree(this.child);
    this.client = null;
    this.child = null;
    this.ready = false;
  }

  async updateGoal(input: GoalUpdate) {
    return this.mutateGoal(() => this.applyGoal(input));
  }

  private async mutateGoal(action: () => Promise<void>) {
    if (this.goalMutating) throw new GoalError("A goal mutation is already pending; retry after it completes", 409);
    this.goalMutating = true;
    try { await action(); } finally { this.goalMutating = false; }
  }

  private async applyGoal(input: GoalUpdate) {
    if (!this.harness.capabilities.goal) throw new GoalError("Codex does not support goals", 501);
    if (!this.client || !this.thread) throw new GoalError("Codex is not ready", 503);
    if (["blocked", "stale", "done"].includes(input.status || "") && input.reason) {
      if (!this.goal) throw new GoalError("No goal set", 409);
      const goal = { ...this.goal, status: input.status!, reason: input.reason, requiredAction: input.requiredAction };
      // Clear the native continuation condition for every formal exit. Preserve
      // the richer launcher outcome separately; native has no stale status.
      this.pendingGoalExit = goal; this.saveGoalExit();
      await this.client.request("thread/goal/clear", { threadId: this.thread.id, origin: "user" });
      this.goal = this.goalExitRecord = goal;
      this.pendingGoalExit = null;
      this.saveGoalExit(); this.emit("state"); this.emit("goal-exit", goal); return;
    }
    const { reason: _reason, requiredAction: _action, roundBudget: _rounds, ...native } = input;
    const changedObjective = input.objective && input.objective !== this.goal?.objective;
    const goalId = changedObjective ? randomUUID() : this.goal?.id || randomUUID();
    if (changedObjective) {
      this.goalGuard = new GoalGuard(undefined, input.roundBudget, input.tokenBudget);
      this.goalTurnsSeen.clear();
      native.tokenBudget = this.goalGuard.tokenBudget;
      native.status = input.status ?? "active";
    } else if (input.roundBudget !== undefined || input.tokenBudget !== undefined) {
      const guard = new GoalGuard(undefined, input.roundBudget !== undefined ? input.roundBudget : this.goal?.roundBudget ?? null, input.tokenBudget !== undefined ? input.tokenBudget : this.goal?.tokenBudget ?? null);
      guard.rounds = this.goalGuard.rounds; this.goalGuard = guard;
    }
    const result = await this.client.request<{ goal: unknown }>("thread/goal/set", { threadId: this.thread.id, origin: "user", ...native });
    this.goalExitRecord = null;
    this.pendingGoalExit = null;
    this.goal = mapGoal(result.goal);
    if (this.goal) { this.goal.id = goalId; this.goal.roundBudget = this.goalGuard.roundBudget; this.goal.roundsUsed = this.goalGuard.rounds; }
    this.saveGoalExit();
    this.emit("state");
  }

  async clearGoal() {
    return this.mutateGoal(() => this.applyClearGoal());
  }

  private async applyClearGoal() {
    if (!this.harness.capabilities.goal) throw new GoalError("Codex does not support goals", 501);
    if (!this.client || !this.thread) throw new GoalError("Codex is not ready", 503);
    const previous = this.goal;
    await this.client.request("thread/goal/clear", { threadId: this.thread.id, origin: "user" });
    this.goal = this.goalExitRecord = previous ? { ...previous, status: "cleared", reason: "Goal cleared by user" } : null;
    this.saveGoalExit();
    this.emit("state");
  }

  private saveGoalExit() {
    if (!this.options.stateFile) return;
    writeGoalState(this.options.stateFile + ".goal-exit.json", { threadId: this.thread?.id, goal: this.goalExitRecord, pendingExit: this.pendingGoalExit, guard: { id: this.goal?.id, objective: this.goal?.objective, roundBudget: this.goalGuard.roundBudget, roundsUsed: this.goalGuard.rounds } });
  }

  snapshot(historyLimit?: number) {
    const thread = this.thread && historyLimit !== undefined
      ? this.threadWithHistoryLimit(historyLimit)
      : this.thread;
    return {
      ready: this.ready,
      thread,
      models: this.models,
      backgroundTasks: [],
      goal: this.goal,
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
    return withHistoryLimit(this.thread!, this.mergedTurns(), historyLimit);
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
    for (const [turnId, plan] of this.turnPlans) {
      let turn = byId.get(turnId);
      if (!turn) {
        turn = { id: turnId, startedAt: null, status: "inProgress", error: null, items: [] };
        turns.push(turn);
        byId.set(turnId, turn);
      }
      turn.items.push(mapItem(plan, turnId, turn.items.length));
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
      if (Array.isArray(record.turns)) this.fullTurns = record.turns.map((turn) => mapTurn(turn));
      // A turn cut off by a restart stays "inProgress" in the persisted history
      // forever. Only the newest turn can be live, and only while the thread is
      // active (or its status is unknown); every other stale turn was interrupted.
      const newest = this.fullTurns.at(-1);
      const threadLive = statusType === null || statusType === undefined || statusType === "active";
      for (const turn of this.fullTurns) {
        if (turn.status === "inProgress" && (turn !== newest || !threadLive)) turn.status = "interrupted";
      }
      const active = newest?.status === "inProgress" ? newest : undefined;
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
    if (method === "thread/goal/updated" || method === "thread/goal/cleared") {
      if (params.threadId !== this.thread?.id) return;
      const previous = this.goal;
      this.goal = method === "thread/goal/cleared" ? this.goalExitRecord : mapGoal(params.goal);
      if (this.goal) {
        this.goal.id = previous?.objective === this.goal.objective ? previous.id : randomUUID();
        this.goal.roundBudget = this.goalGuard.roundBudget; this.goal.roundsUsed = this.goalGuard.rounds;
        if (["budgetLimited", "usageLimited"].includes(this.goal.status)) this.blockForBudget("Native Codex goal budget/usage limit reached");
      }
      this.emit("state");
      return;
    }
    if (method === "turn/plan/updated" && typeof params.turnId === "string") {
      this.turnPlans.set(params.turnId, { id: `plan:${params.turnId}`, type: "plan",
        text: typeof params.explanation === "string" ? params.explanation : "Plan", plan: params.plan });
      this.publishLiveHistory();
      return;
    }
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
    if ((method === "item/agentMessage/delta" || method === "item/reasoning/textDelta" || method === "item/plan/delta")
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
      if (turnId && this.goal?.status === "active" && !this.goalTurnsSeen.has(turnId)) {
        this.goalTurnsSeen.add(turnId); this.goalGuard.rounds++;
        this.goal.roundsUsed = this.goalGuard.rounds;
        this.saveGoalExit();
        if (this.goalGuard.roundBudget !== null && this.goalGuard.rounds >= this.goalGuard.roundBudget) this.blockForBudget("Goal round budget reached");
      }
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

  private blockForBudget(reason: string) {
    if (this.goalBudgetExit) return;
    if (this.goalMutating) {
      const id = this.goal?.id;
      setTimeout(() => { if (this.ready && this.goal?.id === id) this.blockForBudget(reason); }, 25).unref(); return;
    }
    this.goalBudgetExit = true;
    void this.updateGoal({ status: "blocked", reason, requiredAction: "User: review the goal before explicitly setting a new one" })
      .catch((error) => this.emit("log", `Codex goal budget exit failed: ${String(error)}`))
      .finally(() => { this.goalBudgetExit = false; });
  }
}
