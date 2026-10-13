import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { randomUUID } from "node:crypto";
import { EventEmitter } from "node:events";
import { createReadStream } from "node:fs";
import { readFile, readdir, realpath } from "node:fs/promises";
import { readGoalState, writeGoalState } from "../goal-storage.js";
import { homedir, tmpdir } from "node:os";
import { join, relative, resolve } from "node:path";
import { createInterface } from "node:readline";
import { mapPlanSteps, mapReasoningEffort, type BackgroundTask, type Goal, type GoalUpdate, type HistoryItem, type ModelOption, type TurnDto } from "../model.js";
import { GoalError, GoalGuard } from "../goals.js";
import { withHistoryLimit, type HarnessInfo, type HarnessRuntime, type ThreadState } from "../runtime.js";
import { loadThreadId, saveThreadId } from "../thread-state.js";
import { unifiedDiff } from "./diff.js";
import { SessionTransferError, type SessionBundle, type SessionImportOptions } from "../session-transfer.js";
import { exportNativeSession, importNativeSession } from "./session-files.js";

type RecordValue = Record<string, unknown>;
const record = (value: unknown): RecordValue => value && typeof value === "object" && !Array.isArray(value) ? value as RecordValue : {};
const text = (value: unknown) => typeof value === "string" ? value : "";
const projectName = (cwd: string) => cwd.replace(/[^a-zA-Z0-9]/g, "-");
const timestamp = (value: unknown): string | null => {
  if (value == null) return null;
  const date = new Date(typeof value === "number" ? value < 1e12 ? value * 1000 : value : String(value));
  return Number.isFinite(date.getTime()) ? date.toISOString() : null;
};
const contentText = (content: unknown): string => typeof content === "string" ? content : Array.isArray(content)
  ? content.map((part) => text(record(part).text) || text(record(part).thinking)).join("\n") : JSON.stringify(content ?? "");

function structuredDiff(path: string, patch: unknown): string | undefined {
  if (!Array.isArray(patch)) return undefined;
  const hunks = patch.flatMap((entry) => {
    const hunk = record(entry);
    if (!Array.isArray(hunk.lines)) return [];
    return [`@@ -${Number(hunk.oldStart) || 0},${Number(hunk.oldLines) || 0} +${Number(hunk.newStart) || 0},${Number(hunk.newLines) || 0} @@`, ...hunk.lines.filter((line): line is string => typeof line === "string")];
  });
  return hunks.length ? [`--- a/${path}`, `+++ b/${path}`, ...hunks].join("\n") : undefined;
}

/** Maps native Claude content blocks; tool results enrich the same tool-use item. */
export function mapClaudeBlock(block: RecordValue, id: string, cwd: string): HistoryItem {
  if (block.type === "thinking") return { id, kind: "reasoning", text: text(block.thinking) };
  if (block.type === "text") return { id, kind: "agentMessage", text: text(block.text) };
  const name = text(block.name);
  const input = record(block.input);
  if (name === "Bash") return { id, kind: "commandExecution", text: text(input.command), status: "inProgress", detailText: "" };
  if (name === "TodoWrite") return { id, kind: "plan", text: "Plan", steps: mapPlanSteps(input.todos), status: "inProgress" };
  if (["Edit", "Write", "MultiEdit"].includes(name)) {
    const path = text(input.file_path) || text(input.path);
    const displayPath = path.startsWith("/") ? relative(cwd, path) : path;
    const edits = name === "MultiEdit" && Array.isArray(input.edits) ? input.edits.map(record) : [input];
    return { id, kind: "fileChange", text: displayPath, status: "inProgress", changes: edits.map((edit) => ({
      path: displayPath,
      kind: name === "Write" ? "add" : "update",
      diff: structuredDiff(displayPath, edit.structuredPatch ?? input.structuredPatch)
        ?? unifiedDiff(displayPath, name === "Write" ? null : text(edit.old_string), name === "Write" ? text(input.content) : text(edit.new_string)),
    })) };
  }
  return { id, kind: "toolCall", text: name || text(block.type) || "Tool", detailText: JSON.stringify(input, null, 2), status: "inProgress" };
}

export class ClaudeRuntime extends EventEmitter implements HarnessRuntime {
  readonly harness: HarnessInfo = { id: "claude", name: "Claude", capabilities: { steer: true, backgroundTasks: true, goal: { pause: false, budget: true }, sessionTransfer: { export: true, import: true } } };
  private goal: Goal | null = null;
  private goalKey = "";
  private goalHistoryPath = "";
  private goalHistorySeen = new Set<string>();
  private goalHookSeen = new Set<string>();
  private goalUsageSeen = new Set<string>();
  private resultChain = Promise.resolve();
  private goalCommandRunning = false;
  private goalGuard = new GoalGuard();
  private goalExit?: Promise<void>;
  private goalPoll?: ReturnType<typeof setInterval>;
  private goalPolling = false;
  private goalTokenMessages = new Set<string>();
  private queryGoalTokens = 0;
  readonly taskOutputRoots: string[];
  private child: ChildProcessWithoutNullStreams | null = null;
  private ready = false;
  private stopping = false;
  private sessionId = "";
  private thread: ThreadState | null = null;
  private turns: TurnDto[] = [];
  private tasks = new Map<string, BackgroundTask>();
  private models: ModelOption[] = [];
  private modelAliases = new Map<string, string>();
  private settingsChange: Promise<void> = Promise.resolve();
  private effortSupported = false;
  private controls = new Map<string, { resolve: (value: RecordValue) => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> }>();
  private streamMessage = "";
  private streamBlocks = new Map<number, { block: RecordValue; json: string; id: string }>();
  private interrupted = false;
  private pendingUsers: Array<{ text: string; id: string }> = [];

  async exportSession() {
    if (!this.ready || !this.thread) throw new SessionTransferError("not_ready", "No confirmed Claude session to export");
    return exportNativeSession("claude", this.sessionId, this.cwd, this.options.configDir);
  }

  async importSession(bundle: SessionBundle, options: SessionImportOptions) {
    if (bundle.harness !== "claude") throw new SessionTransferError("unsupported", "Claude cannot import another harness session");
    return importNativeSession(bundle, options, this.options.configDir);
  }

  constructor(private command: string, private cwd: string,
    private options: { sessionId?: string; stateFile?: string; model?: string; configDir?: string; args?: string[] } = {}) {
    super();
    const directory = projectName(resolve(cwd));
    const taskRoot = `claude-${process.getuid?.() ?? "unknown"}`;
    this.taskOutputRoots = [resolve(cwd), join(tmpdir(), taskRoot, directory), join("/tmp", taskRoot, directory)];
  }

  async start() {
    const resumed = this.options.sessionId || loadThreadId(this.options.stateFile);
    this.sessionId = resumed || randomUUID();
    if (!/^[a-zA-Z0-9_-]+$/.test(this.sessionId)) throw new Error("Invalid Claude session ID; no new session was created");
    for (let index = 1; index < this.taskOutputRoots.length; index++) this.taskOutputRoots[index] = join(this.taskOutputRoots[index], this.sessionId, "tasks");
    const now = new Date().toISOString();
    this.thread = { id: this.sessionId, title: "Claude", cwd: this.cwd, model: this.options.model ?? null,
      reasoningEffort: null, status: "idle", activeTurnId: null, lastError: null, createdAt: now, updatedAt: now, turns: this.turns, hasOlderItems: false };
    try {
      this.goalHistoryPath = join(this.options.configDir || process.env.CLAUDE_CONFIG_DIR || join(homedir(), ".claude"), "projects", projectName(await realpath(this.cwd)), `${this.sessionId}.jsonl`);
      if (resumed) await this.loadHistory(resumed);
      if (resumed && this.options.stateFile) {
        const saved = record(readGoalState(this.options.stateFile + ".goal-usage.json"));
        if (saved.sessionId === this.sessionId && saved.goalKey === this.goalKey) {
          if (saved.goal && ["blocked", "stale", "done", "cleared"].includes(text(record(saved.goal).status))) this.goal = saved.goal as unknown as Goal;
          if (this.goal) {
            this.goal.tokensUsed = typeof saved.tokensUsed === "number" ? saved.tokensUsed : null;
            this.goal.timeUsedSeconds = typeof saved.timeUsedSeconds === "number" ? saved.timeUsedSeconds : null;
            this.goalGuard = new GoalGuard(undefined, this.goal.roundBudget, this.goal.tokenBudget ?? undefined);
            this.goalGuard.rounds = this.goal.roundsUsed ?? 0;
            this.goalGuard.tokens = this.goal.tokensUsed ?? 0;
            this.goalGuard.totalRejections = this.goal.rejectionCount ?? 0;
            this.goalGuard.rejections = this.goal.consecutiveRejections ?? 0;
          }
        }
      }
      const args = [...(this.options.args ?? []), "--print", "--verbose", "--output-format", "stream-json", "--input-format", "stream-json",
        "--include-partial-messages", "--dangerously-skip-permissions", ...(this.options.model ? ["--model", this.options.model] : []),
        ...(process.env.AGENT_UI_GOAL_COMMAND && process.env.AGENT_UI_GOAL_AGENT_EXIT !== "0" ? ["--append-system-prompt", `You may formally exit a goal using node "$AGENT_UI_GOAL_COMMAND" done|blocked|stale --reason "reason" --required-action "who must do what". Use blocked for external dependencies and stale for obsolete goals; do not repeatedly spend tokens on an impossible goal. You may cite an explicit user request to close as-is in the reason. This launcher command clears the native goal on your behalf.`] : []),
        ...(resumed ? ["--resume", resumed] : ["--session-id", this.sessionId])];
      this.stopping = false;
      const child = spawn(this.command, args, { cwd: this.cwd, stdio: "pipe" });
      this.child = child;
      child.stdin.on("error", (error) => { if (!this.stopping) this.fail(error); });
      createInterface({ input: child.stdout }).on("line", (line) => {
        let event: RecordValue;
        try { event = record(JSON.parse(line)); } catch { this.emit("log", "Ignored non-JSON Claude output"); return; }
        try { this.handle(event); } catch (error) { this.fail(error instanceof Error ? error : new Error(String(error))); }
      });
      child.stderr.on("data", (chunk) => { const message = String(chunk).trim(); if (message) this.emit("log", message); });
      child.on("error", (error) => this.fail(error));
      child.on("exit", (code, signal) => {
        this.ready = false;
        this.rejectControls(new Error(`Claude exited (${code ?? signal})`));
        if (!this.stopping) { this.fail(new Error(`Claude exited (${code ?? signal})`)); this.emit("exit", { code, signal }); }
      });
      const initialized = await this.control({ subtype: "initialize" });
      if (Array.isArray(initialized.commands) && !initialized.commands.some((command) => record(command).name === "goal")) this.harness.capabilities.goal = null;
      this.goalPoll = setInterval(() => {
        if (this.goalPolling || this.goal?.status !== "active") return;
        this.goalPolling = true;
        const reason = this.goalGuard.limit(); if (reason) this.blockGoal(reason);
        void this.refreshGoalHistory().catch((error) => this.emit("log", `Goal monitor: ${error instanceof Error ? error.message : "read failed"}`)).finally(() => { this.goalPolling = false; });
      }, 500);
      this.goalPoll.unref();
      const models = Array.isArray(initialized.models) ? initialized.models : [];
      // Alias values (sonnet) and system/init's actual model IDs differ. Use
      // Claude's own resolution, never a locally pinned model-version table.
      const reported = models.length ? models : ["default", "sonnet", "opus", "haiku"].map((value) => ({ value }));
      const unique = new Map<string, ModelOption>();
      for (const entry of reported) {
        const info = record(entry);
        const alias = text(info.value) || text(info.id);
        if (!alias) continue;
        const model = text(info.resolvedModel) || alias;
        this.modelAliases.set(alias, model);
        const efforts = info.supportsEffort && Array.isArray(info.supportedEffortLevels)
          ? info.supportedEffortLevels.map(mapReasoningEffort).filter((value) => value !== null) : [];
        const option: ModelOption = { id: alias, model, displayName: text(info.displayName) || model, description: text(info.description), isDefault: alias === "default", hidden: false,
          supportedReasoningEfforts: efforts.map((reasoningEffort) => ({ reasoningEffort, description: "" })), defaultReasoningEffort: null };
        if (!unique.has(model) || unique.get(model)!.isDefault) unique.set(model, option);
      }
      this.models = [...unique.values()];
      if (this.models.some((model) => model.supportedReasoningEfforts.length)) {
        // Older CLIs may report model-level effort support without exposing
        // the live setter. An empty, session-only overlay probes it safely.
        try { await this.control({ subtype: "apply_flag_settings", settings: {} }); this.effortSupported = true; }
        catch (error) { this.emit("log", `Claude effort controls unavailable: ${error}`); }
      }
      if (!this.effortSupported) for (const model of this.models) model.supportedReasoningEfforts = [];
      this.setCurrentModel(this.thread.model || this.modelAliases.get("default") || "default");
      // A new --session-id is only reserved until the first user input. Claude
      // has no resumable JSONL yet, so persist it on system/init instead.
      if (resumed) saveThreadId(this.options.stateFile, this.sessionId);
      this.ready = true;
      this.publish();
    } catch (error) {
      await this.stop();
      throw new Error(`${resumed ? `Failed to resume Claude session ${resumed}; no new session was created` : "Failed to start Claude"}: ${error instanceof Error ? error.message : error}`);
    }
  }

  async prompt(value: string) {
    // Keep a prompt submitted during a setting change: send it after the
    // native acknowledgement, even if the change was rejected (old settings).
    await this.settingsChange.catch(() => {});
    if (!this.ready || !this.thread) throw new Error("Claude is not ready");
    if (this.thread.status !== "running") this.beginTurn();
    const id = randomUUID();
    this.turn().items.push({ id, kind: "userMessage", text: value });
    this.pendingUsers.push({ id, text: value });
    this.thread.status = "running";
    this.thread.lastError = null;
    this.publish();
    await this.send({ type: "user", uuid: id, message: { role: "user", content: [{ type: "text", text: value }] } });
  }

  async interrupt() {
    if (!this.ready) throw new Error("Claude is not ready");
    this.interrupted = true;
    await this.control({ subtype: "interrupt" });
  }

  updateSettings(input: { model?: string; reasoningEffort?: string | null }): Promise<void> {
    const change = this.settingsChange.catch(() => {}).then(() => this.changeSettings(input));
    this.settingsChange = change;
    return change;
  }

  private async changeSettings(input: { model?: string; reasoningEffort?: string | null }) {
    const target = this.modelAliases.get(input.model || "") || input.model || this.thread?.model;
    const model = this.models.find((option) => option.model === target);
    if (input.reasoningEffort !== undefined && (!this.effortSupported || input.reasoningEffort !== null && !model?.supportedReasoningEfforts.some((option) => option.reasoningEffort === input.reasoningEffort))) {
      throw new Error("Claude reasoning effort is not supported for this model");
    }
    if (input.model) {
      await this.control({ subtype: "set_model", model: input.model });
      this.setCurrentModel(input.model);
      if (this.thread && !this.models.find((option) => option.model === this.thread!.model)?.supportedReasoningEfforts.some((option) => option.reasoningEffort === this.thread!.reasoningEffort)) this.thread.reasoningEffort = null;
      this.publish();
    }
    if (input.reasoningEffort !== undefined) {
      await this.control({ subtype: "apply_flag_settings", settings: { effortLevel: input.reasoningEffort } });
      if (this.thread) this.thread.reasoningEffort = mapReasoningEffort(input.reasoningEffort);
      this.publish();
    }
  }

  private setCurrentModel(value: string) {
    const model = this.modelAliases.get(value) || value;
    if (this.thread) this.thread.model = model;
    if (!this.models.some((option) => option.model === model)) this.models.push({ id: model, model, displayName: model, description: "Current model reported by Claude", isDefault: false, hidden: false, supportedReasoningEfforts: [], defaultReasoningEffort: null });
  }

  async updateGoal(input: GoalUpdate) {
    if (["blocked", "stale", "done"].includes(input.status || "")) {
      if (!this.goal) throw new GoalError("No goal set", 409);
      if (!input.reason?.trim()) throw new GoalError("A goal exit requires reason");
      if (this.goalExit) return this.goalExit;
      const goal = { ...this.goal, status: input.status!, reason: input.reason, requiredAction: input.requiredAction };
      const exit = async () => {
        // Clearing the native Stop hook is the actual release, not a UI-only status.
        await this.goalCommand("clear");
        this.goal = goal;
        await this.saveGoalUsage(); this.publish(); this.emit("goal-exit", goal);
      };
      this.goalExit = exit().finally(() => { this.goalExit = undefined; });
      return this.goalExit;
    }
    if (input.status !== undefined) throw new GoalError("Claude goals do not support pause/resume or other explicit status", 501);
    if (!input.objective?.trim() || input.objective.trim() === this.goal?.objective && this.goal.status === "active") {
      if (!this.goal) throw new GoalError("objective or a budget update for an existing goal is required");
      const guard = new GoalGuard(undefined, input.roundBudget ?? this.goal.roundBudget, input.tokenBudget ?? this.goal.tokenBudget ?? undefined);
      guard.rounds = this.goalGuard.rounds; guard.tokens = this.goalGuard.tokens;
      guard.rejections = this.goalGuard.rejections; guard.totalRejections = this.goalGuard.totalRejections;
      this.goalGuard = guard; this.goal.tokenBudget = guard.tokenBudget; this.goal.roundBudget = guard.roundBudget;
      await this.saveGoalUsage(); this.publish();
      const reason = guard.limit(); if (reason) this.blockGoal(reason); return;
    }
    // `clear` is a reserved native command, not an objective.
    if (input.objective.trim() === "clear") throw new GoalError("clear is reserved by Claude; use Delete goal");
    await this.goalCommand(input.objective.trim());
    if (this.goal) {
      this.goalGuard = new GoalGuard(undefined, input.roundBudget, input.tokenBudget ?? undefined);
      this.goal.tokenBudget = this.goalGuard.tokenBudget;
      this.goal.roundBudget = this.goalGuard.roundBudget;
      await this.saveGoalUsage(); this.publish();
    }
  }

  async clearGoal() {
    const previous = this.goal;
    await this.goalCommand("clear");
    if (previous) { this.goal = { ...previous, status: "cleared", reason: "Goal cleared by user" }; await this.saveGoalUsage(); this.publish(); }
  }

  private async goalCommand(args: string) {
    if (!this.harness.capabilities.goal) throw new GoalError("This Claude version does not support goals", 501);
    if (this.goalCommandRunning) throw new GoalError("A Claude goal command is already pending", 409);
    this.goalCommandRunning = true;
    let cleanup = () => {};
    const accepted = new Promise<void>((resolve, reject) => {
      const onCommand = (received: string) => { if (received === args) { cleanup(); resolve(); } };
      const onError = (error: Error) => { cleanup(); reject(error); };
      const timer = setTimeout(() => onError(new Error("Claude goal command timed out waiting for native acknowledgement")), 60_000);
      cleanup = () => { clearTimeout(timer); this.off("goal-command", onCommand); this.off("goal-command-error", onError); };
      this.on("goal-command", onCommand); this.on("goal-command-error", onError);
    });
    // Register a rejection handler before send/stop can reject the acknowledgement.
    void accepted.catch(() => {});
    try { await this.prompt(`/goal ${args}`); await accepted; }
    finally { cleanup(); this.goalCommandRunning = false; }
  }

  async stop() {
    if (this.goalPoll) clearInterval(this.goalPoll);
    this.stopping = true;
    this.emit("goal-command-error", new Error("Claude stopped"));
    this.ready = false;
    this.rejectControls(new Error("Claude stopped"));
    const child = this.child;
    this.child = null;
    if (child?.pid && child.exitCode === null && child.signalCode === null) {
      await new Promise<void>((done) => {
        const timer = setTimeout(() => child.kill("SIGKILL"), 2000);
        child.once("close", () => { clearTimeout(timer); done(); });
        child.stdin.end(); child.kill("SIGTERM");
      });
    }
  }

  snapshot(historyLimit?: number) {
    return { ready: this.ready, thread: this.thread && historyLimit !== undefined ? withHistoryLimit(this.thread, this.turns, historyLimit) : this.thread,
      models: this.models, backgroundTasks: [...this.tasks.values()], goal: this.goal };
  }

  private send(value: unknown): Promise<void> {
    const child = this.child;
    if (!child || child.stdin.destroyed) return Promise.reject(new Error("Claude stdin is closed"));
    return new Promise((resolve, reject) => { child.stdin.write(JSON.stringify(value) + "\n", (error) => error ? reject(error) : resolve()); });
  }

  private control(request: RecordValue): Promise<RecordValue> {
    const requestId = randomUUID();
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.controls.delete(requestId); reject(new Error(`Claude ${request.subtype} timed out`)); }, 15_000);
      this.controls.set(requestId, { resolve, reject, timer });
      void this.send({ type: "control_request", request_id: requestId, request }).catch((error: Error) => {
        clearTimeout(timer); this.controls.delete(requestId); reject(error);
      });
    });
  }

  private rejectControls(error: Error) {
    for (const pending of this.controls.values()) { clearTimeout(pending.timer); pending.reject(error); }
    this.controls.clear();
  }

  private fail(error: Error) {
    this.ready = false;
    this.emit("goal-command-error", error);
    this.rejectControls(error);
    this.child?.kill("SIGTERM");
    if (this.thread) { this.thread.status = "error"; this.thread.lastError = error.message; this.thread.activeTurnId = null; }
    const turn = this.turns.at(-1);
    if (turn?.status === "inProgress") { turn.status = "failed"; turn.error = error.message; }
    this.publish();
  }

  private publish() { if (this.thread) this.thread.updatedAt = new Date().toISOString(); this.emit("state"); }

  private beginTurn(startedAt = new Date().toISOString(), historical = false) {
    const turn: TurnDto = { id: randomUUID(), startedAt, status: historical ? "completed" : "inProgress", error: null, items: [] };
    this.turns.push(turn);
    if (this.thread && !historical) { this.thread.status = "running"; this.thread.activeTurnId = turn.id; }
    this.interrupted = false;
    return turn;
  }

  private turn(): TurnDto { return this.turns.at(-1) ?? this.beginTurn(); }

  private put(item: HistoryItem) {
    for (const turn of this.turns) {
      const index = turn.items.findIndex((entry) => entry.id === item.id);
      if (index >= 0) { turn.items[index] = { ...turn.items[index], ...item }; return; }
    }
    this.turn().items.push(item);
  }

  private message(event: RecordValue, historical = false) {
    if (event.isSidechain) return;
    if (!historical) this.goalEvent(event);
    const message = record(event.message);
    if (!historical && event.type === "assistant" && message.model !== "<synthetic>" && this.goal?.status === "active") {
      const id = text(message.id) || text(event.uuid);
      if (id && !this.goalTokenMessages.has(id)) {
        this.goalTokenMessages.add(id);
        const usage = record(message.usage);
        const tokens = ["input_tokens", "output_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"].reduce((sum, key) => sum + (typeof usage[key] === "number" ? usage[key] as number : 0), 0);
        this.queryGoalTokens += tokens;
        const reason = this.goalGuard.usage(tokens);
        this.goal.tokensUsed = this.goalGuard.tokens;
        if (reason) this.blockGoal(reason);
      }
    }
    if (!historical && event.type === "assistant" && typeof message.model === "string" && message.model !== "<synthetic>") this.setCurrentModel(message.model);
    if (!historical && message.model === "<synthetic>" && typeof event.local_command_source === "string" && this.thread) {
      const command = record(event.local_command_run);
      const value = contentText(message.content);
      if (command.command === "effort" && /^Set effort level to /.test(value)) this.thread.reasoningEffort = mapReasoningEffort(text(command.args));
      if (command.command === "model") {
        const effort = /\(effort: (low|medium|high|xhigh|max)\)/.exec(value);
        if (effort) this.thread.reasoningEffort = mapReasoningEffort(effort[1]);
      }
    }
    const blocks = typeof message.content === "string" ? [{ type: "text", text: message.content }] : Array.isArray(message.content) ? message.content.map(record) : [];
    const isUser = event.type === "user";
    if (isUser && blocks.some((block) => block.type !== "tool_result")) {
      const value = blocks.filter((block) => block.type === "text").map((block) => text(block.text)).join("\n");
      const pending = !historical && this.pendingUsers.findIndex((entry) => entry.text === value);
      if (typeof pending === "number" && pending >= 0) this.pendingUsers.splice(pending, 1);
      else {
        if (historical || this.thread?.status !== "running") this.beginTurn(text(event.timestamp) || undefined, historical);
        this.put({ id: text(event.uuid) || randomUUID(), kind: "userMessage", text: value });
      }
    }
    for (const [index, block] of blocks.entries()) {
      if (!historical && ["tool_use", "tool_result"].includes(text(block.type)) && this.goal?.status === "active") {
        // Ignore random IDs; identical tools/results are not new evidence.
        this.goalGuard.progress(JSON.stringify({ type: block.type, name: block.name, input: block.input, content: block.content, result: event.tool_use_result }));
      }
      if (block.type === "tool_result") {
        const id = text(block.tool_use_id);
        const item = this.turns.flatMap((turn) => turn.items).find((entry) => entry.id === id);
        if (item) {
          item.status = block.is_error ? "failed" : "completed";
          item.detailText = contentText(block.content);
          const result = record(event.toolUseResult ?? event.tool_use_result);
          if (item.kind === "fileChange" && item.changes?.length) {
            const diff = structuredDiff(item.changes[0].path, result.structuredPatch);
            if (diff) item.changes = [{ ...item.changes[0], diff }];
            else if (typeof result.originalFile === "string" && typeof result.content === "string") item.changes = [{ ...item.changes[0], diff: unifiedDiff(item.changes[0].path, result.originalFile, result.content) }];
          }
        }
      } else if (!isUser) {
        // Claude can emit one assistant envelope per block with the same message
        // id. Match the streamed block's real index instead of overwriting index 0.
        const streamed = !historical && text(message.id) === this.streamMessage
          ? [...this.streamBlocks.values()].find((entry) => entry.block.type === block.type
            && (block.type === "text" ? entry.block.text === block.text : block.type === "thinking" ? entry.block.thinking === block.thinking : false)) : undefined;
        const id = text(block.id) || streamed?.id || `${text(event.uuid) || text(message.id) || randomUUID()}:${index}`;
        this.put(mapClaudeBlock(block, id, this.cwd));
      }
    }
  }

  private stream(event: RecordValue) {
    const index = Number(event.index) || 0;
    if (event.type === "message_start") { this.streamMessage = text(record(event.message).id) || randomUUID(); this.streamBlocks.clear(); }
    if (event.type === "content_block_start") {
      const block = record(event.content_block);
      this.streamBlocks.set(index, { block: { ...block }, json: "", id: text(block.id) || `${this.streamMessage}:${index}` });
    }
    const entry = this.streamBlocks.get(index);
    if (!entry) return;
    const delta = record(event.delta);
    if (delta.type === "text_delta") entry.block.text = text(entry.block.text) + text(delta.text);
    if (delta.type === "thinking_delta") entry.block.thinking = text(entry.block.thinking) + text(delta.thinking);
    if (delta.type === "input_json_delta") {
      entry.json += text(delta.partial_json);
      try { entry.block.input = JSON.parse(entry.json); } catch { /* The full tool input arrives at block stop. */ }
    }
    if (["text", "thinking", "tool_use"].includes(text(entry.block.type))) this.put(mapClaudeBlock(entry.block, entry.id, this.cwd));
  }

  private background(event: RecordValue) {
    const update = (value: RecordValue) => {
      const id = text(value.task_id);
      if (!id) return;
      const patch = { ...value, ...record(value.patch) };
      const previous = this.tasks.get(id);
      const rawStatus = text(patch.status);
      const status: BackgroundTask["status"] = ["failed", "error"].includes(rawStatus) ? "failed"
        : ["stopped", "killed", "cancelled", "canceled"].includes(rawStatus) ? "stopped"
          : ["completed", "done", "success"].includes(rawStatus) ? "completed" : ["running", "in_progress"].includes(rawStatus) ? "running" : previous?.status ?? "running";
      const kind = text(patch.task_type);
      const summaryExitCode = /exit code (-?\d+)/i.exec(text(patch.summary));
      this.tasks.set(id, { id, kind: kind.includes("bash") || kind === "shell" ? "shell" : kind.includes("agent") ? "agent" : kind.includes("monitor") ? "monitor" : previous?.kind ?? "other",
        description: text(patch.description) || previous?.description || id, status,
        startedAt: timestamp(patch.start_time) ?? previous?.startedAt ?? new Date().toISOString(),
        endedAt: timestamp(patch.end_time) ?? previous?.endedAt ?? (status === "running" ? null : new Date().toISOString()),
        exitCode: typeof patch.exit_code === "number" ? patch.exit_code : previous?.exitCode ?? (summaryExitCode ? Number(summaryExitCode[1]) : null),
        summary: text(patch.summary) || previous?.summary || null,
        ...(text(patch.output_file) || previous?.outputFile ? { outputFile: text(patch.output_file) || previous?.outputFile }
          : kind === "local_bash" && (patch.is_backgrounded === true || event.subtype === "background_tasks_changed")
            && /^[a-zA-Z0-9_-]+$/.test(id) ? { outputFile: join(this.taskOutputRoots.at(-1)!, `${id}.output`) } : {}),
      });
    };
    if (event.subtype === "background_tasks_changed" && Array.isArray(event.tasks)) for (const task of event.tasks) update(record(task));
    else update(event);
    const finished = [...this.tasks.values()].filter((task) => task.status !== "running").sort((a, b) => (a.endedAt ?? "").localeCompare(b.endedAt ?? ""));
    for (const task of finished.slice(0, Math.max(0, finished.length - 100))) this.tasks.delete(task.id);
  }

  private handle(event: RecordValue) {
    if (event.type === "attachment") this.goalEvent(event);
    if (event.type === "control_response") {
      const response = record(event.response);
      const pending = this.controls.get(text(response.request_id));
      if (pending) { clearTimeout(pending.timer); this.controls.delete(text(response.request_id));
        if (response.subtype === "error") pending.reject(new Error(text(response.error) || "Claude control request failed"));
        else pending.resolve(record(response.response)); }
      return;
    }
    if (event.type === "system") {
      this.goalEvent(event);
      if (event.subtype === "init") {
        if (event.session_id && event.session_id !== this.sessionId) throw new Error(`Claude returned a different session (${event.session_id}); refusing replacement`);
        saveThreadId(this.options.stateFile, this.sessionId);
        if (typeof event.model === "string") this.setCurrentModel(event.model);
      }
      if (["background_tasks_changed", "task_started", "task_updated", "task_notification"].includes(text(event.subtype))) this.background(event);
      if (event.subtype === "session_state_changed") {
        const state = text(event.state) || text(event.session_state);
        if (this.thread && state === "running" && this.thread.status !== "running") this.beginTurn();
        // `result` is the turn boundary. An idle notification can precede it;
        // draining on that notification would let the old result finish a new turn.
        if (this.thread && state === "idle" && !this.thread.activeTurnId) this.thread.status = "idle";
      }
    }
    if (event.type === "assistant" || event.type === "user") this.message(event);
    if (event.type === "stream_event") {
      if (this.thread?.status !== "running") this.beginTurn();
      this.stream(record(event.event));
    }
    if (event.type === "result") {
      this.resultChain = this.resultChain.then(async () => {
        const turn = this.turns.at(-1);
        if (text(event.result) && turn && !turn.items.some((item) => item.kind === "agentMessage" && item.text === event.result)) this.put({ id: text(event.uuid) || randomUUID(), kind: "agentMessage", text: text(event.result) });
        // result.usage is per query; modelUsage is cumulative and must not be summed.
        if (this.goal?.status === "active") {
          const usage = record(event.usage);
          const tokens = ["input_tokens", "output_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"].reduce((sum, key) => sum + (typeof usage[key] === "number" ? usage[key] as number : 0), 0);
          const budgetReason = this.goalGuard.usage(Math.max(0, tokens - this.queryGoalTokens));
          this.goal.tokensUsed = this.goalGuard.tokens;
          this.queryGoalTokens = 0;
          if (budgetReason) this.blockGoal(budgetReason);
          if (this.goal.timeUsedSeconds !== null && typeof event.duration_ms === "number") this.goal.timeUsedSeconds += event.duration_ms / 1000;
        }
        await this.refreshGoalHistory();
        await this.saveGoalUsage();
        this.finish(Boolean(event.is_error), text(event.result) || (Array.isArray(event.errors) ? event.errors.join("\n") : "Claude turn failed"));
        this.publish();
      }).catch((error: Error) => this.fail(error));
      return;
    }
    this.publish();
  }

  private goalEvent(event: RecordValue, historical = false) {
    if (event.isSidechain || event.parent_tool_use_id) return;
    const attachment = record(event.attachment);
    if (event.type === "attachment" && attachment.type === "goal_status" && this.goal?.status === "active") {
      const state = text(attachment.status);
      if (["impossible", "failed", "met", "achieved", "cleared"].includes(state)) {
        this.goal = { ...this.goal, status: ["impossible", "failed"].includes(state) ? "blocked" : state === "cleared" ? "cleared" : "done", reason: text(attachment.reason) || `Native evaluator reported ${state}` };
        if (!historical && this.goal.status === "blocked") this.emit("goal-exit", this.goal);
      }
    }
    const message = record(event.message);
    const command = record(event.commandRun ?? event.local_command_run);
    const local = event.type === "system" && event.subtype === "local_command" && command.command === "goal"
      || event.type === "assistant" && message.model === "<synthetic>" && command.command === "goal" && typeof event.local_command_source === "string";
    const key = text(event.uuid);
    if (local && (!key || !this.goalHistorySeen.has(key))) {
      if (key) this.goalHistorySeen.add(key);
      const value = event.type === "system" ? text(event.content).replace(/^<local-command-stdout>|<\/local-command-stdout>$/g, "") : contentText(message.content);
      const set = /^Goal set: ([\s\S]+)$/.exec(value.trim());
      if (set) {
        this.goal = { id: key || randomUUID(), objective: set[1], status: "active", tokenBudget: null, tokensUsed: 0, timeUsedSeconds: historical ? null : 0 };
        this.goalGuard = new GoalGuard();
        this.goalTokenMessages.clear(); this.queryGoalTokens = 0;
        this.goal.tokenBudget = this.goalGuard.tokenBudget;
        this.goal.roundBudget = this.goalGuard.roundBudget;
        this.goalKey = key;
        this.goalUsageSeen.clear();
      } else if (/^(?:Goal cleared|No goal set)/.test(value) && text(command.args).trim() === "clear" && this.goal) this.goal = { ...this.goal, status: "cleared", reason: "Native goal cleared" };
      if (!historical) this.emit("goal-command", text(command.args).trim());
    }
    if (event.type === "system" && event.subtype === "stop_hook_summary") {
      const hookKey = key || JSON.stringify(event);
      if (this.goalHookSeen.has(hookKey)) return;
      this.goalHookSeen.add(hookKey);
      if (this.goal?.status !== "active") return;
      const matching = Array.isArray(event.hookInfos) && event.hookInfos.some((hook) => record(hook).promptText === this.goal!.objective);
      const impossible = matching && Array.isArray(event.hookInfos) && event.hookInfos.map(record).map((hook) => record(hook.result)).find((result) => result.ok === false && result.impossible === true);
      if (impossible && !historical) { this.blockGoal(text(impossible.reason) || "Native evaluator reported an impossible goal"); return; }
      if (matching && event.preventedContinuation === true && !historical) {
        const reason = this.goalGuard.reject();
        this.goal.roundsUsed = this.goalGuard.rounds;
        this.goal.rejectionCount = this.goalGuard.totalRejections;
        this.goal.consecutiveRejections = this.goalGuard.rejections;
        if (reason) this.blockGoal(reason);
        void this.saveGoalUsage().catch((error) => this.emit("log", `Goal checkpoint failed: ${String(error)}`));
        this.publish();
      }
      // Verified Claude 2.1.296: a successful condition check has no feedback,
      // no hook errors, and does not prevent continuation. Block-cap overrides
      // have hasOutput=true and must never count as goal completion.
      if (matching && event.hasOutput === false && event.preventedContinuation === false && Array.isArray(event.hookErrors) && !event.hookErrors.length) this.goal.status = "done";
    }
    if (historical && event.type === "assistant" && message.model !== "<synthetic>" && this.goal?.status === "active") {
      const id = text(message.id) || key;
      if (id && !this.goalUsageSeen.has(id)) {
        this.goalUsageSeen.add(id);
        const usage = record(message.usage);
        this.goal.tokensUsed = (this.goal.tokensUsed ?? 0) + ["input_tokens", "output_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"].reduce((sum, key) => sum + (typeof usage[key] === "number" ? usage[key] as number : 0), 0);
      }
    }
  }

  private async refreshGoalHistory() {
    const history = await readFile(this.goalHistoryPath, "utf8").catch((error: NodeJS.ErrnoException) => { if (error.code === "ENOENT") return ""; throw error; });
    for (const line of history.split("\n")) {
      let event: RecordValue; try { event = record(JSON.parse(line)); } catch { continue; }
      // Live usage comes only from result, never replay assistant usage twice.
      if (event.type === "system" || event.type === "attachment") this.goalEvent(event);
    }
  }

  private async saveGoalUsage() {
    if (!this.options.stateFile || !this.goal) return;
    const path = this.options.stateFile + ".goal-usage.json";
    writeGoalState(path, { sessionId: this.sessionId, goalKey: this.goalKey, goal: this.goal, tokensUsed: this.goal.tokensUsed, timeUsedSeconds: this.goal.timeUsedSeconds });
  }

  private blockGoal(reason: string) {
    if (this.goalExit || this.goalCommandRunning || this.goal?.status !== "active") return;
    void this.updateGoal({ status: "blocked", reason, requiredAction: "User: review the goal and external dependencies before explicitly setting a new goal" })
      .catch((error) => {
        // Native hooks still exist if clearing failed; never claim release succeeded.
        this.emit("log", `Goal exit failed; Stop hook may still be active: ${String(error)}`);
        this.emit("goal-exit-error", { ...this.goal, reason: `Unable to release Stop hook: ${String(error)}` });
      });
  }

  private finish(failed: boolean, error = "") {
    // Claude reports an interrupted query as an error result; cancellation is idle.
    failed = failed && !this.interrupted;
    const turn = this.turns.at(-1);
    if (turn) { turn.status = this.interrupted ? "interrupted" : failed ? "failed" : "completed"; turn.error = failed ? error : null; }
    if (this.thread) { this.thread.status = failed ? "error" : "idle"; this.thread.activeTurnId = null; this.thread.lastError = failed ? error : null; }
    this.pendingUsers = [];
    this.interrupted = false;
  }

  private async loadHistory(sessionId: string) {
    const config = this.options.configDir || process.env.CLAUDE_CONFIG_DIR || join(homedir(), ".claude");
    const root = join(config, "projects");
    const directory = projectName(await realpath(this.cwd));
    let path = join(root, directory, `${sessionId}.jsonl`);
    try { await realpath(path); }
    catch {
      const matches: string[] = [];
      for (const entry of await readdir(root, { withFileTypes: true }).catch(() => [])) {
        if (!entry.isDirectory()) continue;
        const candidate = join(root, entry.name, `${sessionId}.jsonl`);
        if (await realpath(candidate).catch(() => null)) matches.push(candidate);
      }
      if (matches.length !== 1) throw new Error("Session history was not found uniquely in Claude projects");
      path = matches[0];
    }
    const seen = new Set<string>();
    this.goalHistoryPath = path;
    for await (const line of createInterface({ input: createReadStream(path), crlfDelay: Infinity })) {
      if (!line.trim()) continue;
      const event = record(JSON.parse(line));
      this.goalEvent(event, true);
      const message = record(event.message);
      if (!this.options.model && event.type === "assistant" && typeof message.model === "string" && message.model !== "<synthetic>" && this.thread) this.thread.model = message.model;
      if (event.type !== "user" && event.type !== "assistant") continue;
      if (event.uuid && seen.has(text(event.uuid))) continue;
      seen.add(text(event.uuid));
      this.message(event, true);
    }
    // History has no live turn or background-task state after a restart.
    if (this.thread) { this.thread.status = "idle"; this.thread.activeTurnId = null; }
  }
}
