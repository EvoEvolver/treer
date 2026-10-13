import type { ChildProcessWithoutNullStreams } from "node:child_process";
import { spawnProvider, stopProcessTree } from "../../scripts/process.mjs";
import { EventEmitter } from "node:events";
import { isAbsolute, relative } from "node:path";

import { JsonRpcClient } from "../jsonrpc.js";
import { mapPlanSteps, mapReasoningEffort, type HistoryItem, type ModelOption, type TurnDto } from "../model.js";
import { withHistoryLimit, type HarnessInfo, type HarnessRuntime, type ThreadState } from "../runtime.js";
import { loadThreadId, saveThreadId } from "../thread-state.js";
import { unifiedDiff } from "./diff.js";

// Adapter for any Agent Client Protocol (ACP) agent: claude-agent-acp, Gemini,
// Grok, Cursor, OpenCode, ... The agent runs its own tools; this client only
// approves permission requests and renders session/update notifications.

const PROMPT_TIMEOUT_MS = 24 * 60 * 60 * 1000;
const LOAD_TIMEOUT_MS = 10 * 60 * 1000;
/** Permission modes that let the agent act without asking, by common ACP adapters. */
const FULL_ACCESS_MODES = ["bypassPermissions", "agent-full-access", "full-access", "yolo"];

type Json = Record<string, unknown>;

interface ConfigOption {
  id: string;
  category?: string;
  currentValue?: unknown;
  options?: Array<{ value?: unknown; name?: string; description?: string }>;
}

interface ToolCallRecord {
  toolCallId: string;
  title?: string;
  kind?: string;
  status?: string;
  content?: Json[];
  rawInput?: unknown;
  rawOutput?: unknown;
}

function text(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function contentText(content: unknown): string {
  if (!content || typeof content !== "object") return "";
  const block = content as Json;
  if (block.type === "text") return text(block.text);
  if (block.type === "resource_link") return text(block.uri);
  if (block.type === "resource" && block.resource && typeof block.resource === "object") return text((block.resource as Json).text);
  return "";
}

function stringify(value: unknown) {
  if (value == null || value === "") return "";
  return typeof value === "string" ? value : JSON.stringify(value, null, 2);
}

/** Shows paths inside the workspace relative to it, as the file browser does. */
function displayPath(path: string, cwd: string) {
  if (!isAbsolute(path)) return path;
  const inside = relative(cwd, path);
  return inside && !inside.startsWith("..") && !isAbsolute(inside) ? inside : path;
}

/** Removes a Markdown code fence that some adapters wrap around command output. */
function unfence(output: string) {
  const match = /^```[\w-]*\n([\s\S]*?)\n?```\s*$/.exec(output.trim());
  return match ? match[1] : output;
}

function toolStatus(status: unknown) {
  switch (status) {
    case "pending":
    case "in_progress": return "inProgress";
    case "failed": return "failed";
    case "completed": return "completed";
    default: return null;
  }
}

/** Maps one ACP tool call to the shared history item. */
export function mapToolCall(call: ToolCallRecord, turnId: string, order: number, cwd = "/"): HistoryItem {
  const base = { id: call.toolCallId, transcriptOrder: order, sourceTurnId: turnId, status: toolStatus(call.status) };
  const title = call.title?.trim() || "Tool call";
  const blocks = Array.isArray(call.content) ? call.content : [];
  const diffs = blocks.filter((block) => block.type === "diff" && typeof block.path === "string");
  const output = blocks
    .filter((block) => block.type === "content")
    .map((block) => contentText(block.content))
    .filter(Boolean)
    .join("\n");
  const input = call.rawInput && typeof call.rawInput === "object" ? call.rawInput as Json : {};
  if (diffs.length > 0) {
    return {
      ...base,
      kind: "fileChange",
      text: title,
      changes: diffs.map((block) => {
        const oldText = typeof block.oldText === "string" ? block.oldText : null;
        const path = displayPath(block.path as string, cwd);
        return { path, kind: oldText == null ? "add" : "update", diff: unifiedDiff(path, oldText, text(block.newText)) };
      }),
    };
  }
  if (call.kind === "execute") {
    const command = text(input.command) || title;
    return {
      ...base,
      kind: "commandExecution",
      text: command,
      previewText: command.slice(0, 160),
      detailText: unfence(output || stringify(call.rawOutput)),
    };
  }
  return {
    ...base,
    kind: "toolCall",
    text: title,
    detailText: [
      Object.keys(input).length ? `Arguments\n${stringify(input)}` : "",
      output || call.rawOutput != null ? `Result\n${unfence(output || stringify(call.rawOutput))}` : "",
    ].filter(Boolean).join("\n\n"),
  };
}

export class AcpRuntime extends EventEmitter implements HarnessRuntime {
  harness: HarnessInfo;
  private child: ChildProcessWithoutNullStreams | null = null;
  private client: JsonRpcClient | null = null;
  private ready = false;
  private sessionId: string | null = null;
  private turns: TurnDto[] = [];
  private toolCalls = new Map<string, { turn: TurnDto; record: ToolCallRecord }>();
  private replaying = false;
  private replayTurns = 0;
  private liveTurns = 0;
  private runningPrompts = 0;
  private configOptions: ConfigOption[] = [];
  thread: ThreadState | null = null;
  models: ModelOption[] = [];

  constructor(
    private readonly command: string,
    private readonly args: string[],
    private readonly cwd: string,
    private readonly options: { sessionId?: string; stateFile?: string; name?: string } = {},
  ) {
    super();
    this.harness = { id: "acp", name: options.name || "Agent", capabilities: { steer: false, backgroundTasks: false, goal: null } };
  }

  async start() {
    const sessionId = this.options.sessionId || loadThreadId(this.options.stateFile);
    const child = spawnProvider(this.command, this.args, { stdio: ["pipe", "pipe", "pipe"], cwd: this.cwd });
    this.child = child;
    const client = new JsonRpcClient(child.stdout, child.stdin);
    this.client = client;
    child.on("error", (error) => { client.close(); this.emit("log", `ACP agent failed: ${error.message}`); });
    child.stderr.on("data", (chunk) => {
      const line = chunk.toString().trim();
      if (line) this.emit("log", line);
    });
    child.on("exit", (code, signal) => {
      this.ready = false;
      this.emit("exit", { code, signal });
    });
    client.on("notification", (event: { method?: string; params?: Json }) => {
      if (event.method === "session/update" && event.params?.update && typeof event.params.update === "object") {
        this.onUpdate(event.params.update as Json);
      }
    });
    client.on("request", (request: { id: number; method?: string; params?: Json }) => this.onRequest(request));

    const initialized = await client.request<Json>("initialize", {
      protocolVersion: 1,
      // The agent uses its own file and terminal tools; Treer only renders them.
      clientCapabilities: { fs: { readTextFile: false, writeTextFile: false }, terminal: false },
      clientInfo: { name: "treer-agent-ui", title: "Treer Agent UI", version: "0.2.0" },
    }, 60_000);
    const agentInfo = initialized.agentInfo && typeof initialized.agentInfo === "object" ? initialized.agentInfo as Json : {};
    if (!this.options.name) this.harness = { ...this.harness, name: text(agentInfo.title) || text(agentInfo.name) || "Agent" };
    const capabilities = initialized.agentCapabilities && typeof initialized.agentCapabilities === "object"
      ? initialized.agentCapabilities as Json
      : {};

    let session: Json;
    if (sessionId) {
      if (capabilities.loadSession !== true) {
        throw new Error(`${this.harness.name} cannot load ACP session ${sessionId}; no new session was created`);
      }
      this.replaying = true;
      try {
        session = await client.request<Json>("session/load", { sessionId, cwd: this.cwd, mcpServers: [] }, LOAD_TIMEOUT_MS);
      } catch (error) {
        throw new Error(`Failed to load ${this.harness.name} session ${sessionId}; no new session was created: ${error instanceof Error ? error.message : String(error)}`);
      } finally {
        this.replaying = false;
      }
      this.sessionId = sessionId;
    } else {
      session = await client.request<Json>("session/new", { cwd: this.cwd, mcpServers: [] }, 120_000);
      if (typeof session.sessionId !== "string" || !session.sessionId) throw new Error("ACP session/new did not return a sessionId");
      this.sessionId = session.sessionId;
    }
    saveThreadId(this.options.stateFile, this.sessionId);
    for (const turn of this.turns) if (turn.status === "inProgress") turn.status = "completed";
    this.readSettings(session);
    await this.enableFullAccess(session);

    const now = new Date().toISOString();
    const current = this.models.find((option) => option.isDefault) ?? null;
    this.thread = {
      id: this.sessionId,
      title: this.harness.name,
      cwd: this.cwd,
      model: current?.model ?? null,
      reasoningEffort: this.currentEffort(),
      status: "idle",
      activeTurnId: null,
      lastError: null,
      createdAt: now,
      updatedAt: now,
      turns: this.turns,
      hasOlderItems: false,
    };
    this.ready = true;
    this.emit("state");
  }

  async prompt(input: string) {
    if (!this.client || !this.thread || !this.sessionId) throw new Error(`${this.harness.name} is not ready`);
    const turn: TurnDto = { id: `turn-${++this.liveTurns}`, startedAt: new Date().toISOString(), status: "inProgress", error: null, items: [] };
    turn.items.push({ id: `${turn.id}:user`, kind: "userMessage", text: input, transcriptOrder: 0, sourceTurnId: turn.id });
    this.turns.push(turn);
    this.runningPrompts += 1;
    this.setRunning(turn.id);
    // ACP session/prompt resolves when the turn ends; AIS prompt submission is
    // accepted as soon as the request is sent. Agents that support queueing
    // accept a prompt sent while another turn runs.
    this.client.request<Json>("session/prompt", {
      sessionId: this.sessionId,
      prompt: [{ type: "text", text: input }],
    }, PROMPT_TIMEOUT_MS).then((result) => {
      turn.status = result.stopReason === "cancelled" ? "interrupted" : "completed";
      if (result.stopReason === "refusal") turn.error = `${this.harness.name} refused the request`;
    }, (error: unknown) => {
      turn.status = "failed";
      turn.error = error instanceof Error ? error.message : String(error);
    }).finally(() => {
      for (const item of turn.items) if (item.status === "inProgress") item.status = turn.status === "failed" ? "failed" : "completed";
      this.runningPrompts -= 1;
      if (this.thread) {
        this.thread.lastError = turn.error;
        if (this.runningPrompts === 0) {
          this.thread.status = turn.status === "failed" ? "error" : "idle";
          this.thread.activeTurnId = null;
        }
      }
      this.publish();
    });
    this.publish();
  }

  async interrupt() {
    if (!this.client || !this.sessionId || this.runningPrompts === 0) return;
    this.client.notify("session/cancel", { sessionId: this.sessionId });
  }

  async updateSettings(input: { model?: string; reasoningEffort?: string | null }) {
    if (!this.client || !this.thread || !this.sessionId) throw new Error(`${this.harness.name} is not ready`);
    const model = this.configOptions.find((option) => option.category === "model");
    if (model && typeof input.model === "string" && input.model && input.model !== this.thread.model) {
      const result = await this.client.request<Json>("session/set_config_option", { sessionId: this.sessionId, configId: model.id, value: input.model });
      this.readSettings(result);
      this.thread.model = input.model;
    }
    const effort = this.effortOption();
    // null selects the agent's own default level when it offers one.
    const effortValue = input.reasoningEffort === null
      ? effort?.options?.map((option) => option.value).find((value) => value === "default")
      : input.reasoningEffort;
    if (effort && typeof effortValue === "string" && effortValue && mapReasoningEffort(effortValue) !== this.thread.reasoningEffort) {
      const result = await this.client.request<Json>("session/set_config_option", { sessionId: this.sessionId, configId: effort.id, value: effortValue });
      this.readSettings(result);
      this.thread.reasoningEffort = mapReasoningEffort(effortValue);
    }
    this.thread.updatedAt = new Date().toISOString();
    this.emit("state");
  }

  async stop() {
    this.client?.close();
    stopProcessTree(this.child);
    this.client = null;
    this.child = null;
    this.ready = false;
  }

  snapshot(historyLimit?: number) {
    const thread = this.thread && historyLimit !== undefined ? withHistoryLimit(this.thread, this.turns, historyLimit) : this.thread;
    return { ready: this.ready, thread, models: this.models, backgroundTasks: [], goal: null };
  }

  private publish() {
    if (!this.thread) return;
    this.thread.turns = this.turns;
    this.thread.updatedAt = new Date().toISOString();
    this.emit("state");
  }

  private setRunning(turnId: string) {
    if (!this.thread) return;
    this.thread.status = "running";
    this.thread.activeTurnId = turnId;
    this.thread.lastError = null;
  }

  private onRequest(request: { id: number; method?: string; params?: Json }) {
    const client = this.client;
    if (!client) return;
    if (request.method === "session/request_permission") {
      const options = Array.isArray(request.params?.options) ? request.params.options as Json[] : [];
      const choice = options.find((option) => option.kind === "allow_always")
        ?? options.find((option) => option.kind === "allow_once")
        ?? options[0];
      client.respond(request.id, choice
        ? { outcome: { outcome: "selected", optionId: choice.optionId } }
        : { outcome: { outcome: "cancelled" } });
      return;
    }
    // fs/* and terminal/* are not advertised, so the agent should not call them.
    client.respondError(request.id, -32601, `${request.method ?? "request"} is not supported by Treer Agent UI`);
  }

  private currentTurn(): TurnDto {
    let turn = this.turns.at(-1);
    if (!turn) {
      turn = { id: `history-${++this.replayTurns}`, startedAt: null, status: "completed", error: null, items: [] };
      this.turns.push(turn);
    }
    return turn;
  }

  private appendChunk(kind: "userMessage" | "agentMessage" | "reasoning", update: Json) {
    const chunk = contentText(update.content);
    if (!chunk) return;
    const messageId = text(update.messageId);
    if (kind === "userMessage") {
      if (!this.replaying) return; // live prompts are recorded by prompt()
      const last = this.turns.at(-1);
      const lastItem = last?.items.at(-1);
      if (last && lastItem?.kind === "userMessage" && messageId && lastItem.id === messageId) {
        lastItem.text += chunk;
      } else {
        const turn: TurnDto = { id: `history-${++this.replayTurns}`, startedAt: null, status: "completed", error: null, items: [] };
        turn.items.push({ id: messageId || `${turn.id}:user`, kind, text: chunk, transcriptOrder: 0, sourceTurnId: turn.id });
        this.turns.push(turn);
      }
      return;
    }
    const turn = this.currentTurn();
    const last = turn.items.at(-1);
    const id = messageId ? `${messageId}:${kind}` : null;
    if (last && last.kind === kind && (id ? last.id === id : true)) {
      last.text += chunk;
    } else {
      turn.items.push({ id: id ?? `${turn.id}:${turn.items.length}`, kind, text: chunk, transcriptOrder: turn.items.length, sourceTurnId: turn.id });
    }
  }

  private upsertToolCall(update: Json, created: boolean) {
    const toolCallId = text(update.toolCallId);
    if (!toolCallId) return;
    const existing = this.toolCalls.get(toolCallId);
    const turn = existing?.turn ?? this.currentTurn();
    const record: ToolCallRecord = { ...(existing?.record ?? { toolCallId }) };
    for (const key of ["title", "kind", "status", "content", "rawInput", "rawOutput"] as const) {
      if (update[key] !== undefined && update[key] !== null) (record as unknown as Json)[key] = update[key];
    }
    if (!existing && !created && !record.title) record.title = "Tool call";
    this.toolCalls.set(toolCallId, { turn, record });
    const index = turn.items.findIndex((item) => item.id === toolCallId);
    const item = mapToolCall(record, turn.id, index >= 0 ? index : turn.items.length, this.cwd);
    if (index >= 0) turn.items[index] = item;
    else turn.items.push(item);
  }

  private onUpdate(update: Json) {
    switch (update.sessionUpdate) {
      case "user_message_chunk": this.appendChunk("userMessage", update); break;
      case "agent_message_chunk": this.appendChunk("agentMessage", update); break;
      case "agent_thought_chunk": this.appendChunk("reasoning", update); break;
      case "tool_call": this.upsertToolCall(update, true); break;
      case "tool_call_update": this.upsertToolCall(update, false); break;
      case "plan": {
        const turn = this.currentTurn();
        const entries = Array.isArray(update.entries) ? update.entries as Json[] : [];
        const steps = mapPlanSteps(entries.map((entry) => ({ step: entry.content, status: entry.status })));
        const id = `${turn.id}:plan`;
        const index = turn.items.findIndex((item) => item.id === id);
        const item: HistoryItem = { id, kind: "plan", text: "Plan", steps, transcriptOrder: index >= 0 ? index : turn.items.length, sourceTurnId: turn.id };
        if (index >= 0) turn.items[index] = item;
        else turn.items.push(item);
        break;
      }
      case "config_option_update": this.readSettings(update); return;
      default: return;
    }
    if (!this.replaying) this.publish();
  }

  private readSettings(source: Json) {
    if (Array.isArray(source.configOptions)) this.configOptions = source.configOptions as ConfigOption[];
    const model = this.configOptions.find((option) => option.category === "model");
    if (model && Array.isArray(model.options)) {
      const efforts = this.effortOption()?.options ?? [];
      const supportedReasoningEfforts = efforts.flatMap((option) => {
        const effort = mapReasoningEffort(option.value);
        return effort ? [{ reasoningEffort: effort, description: text(option.description) }] : [];
      });
      this.models = model.options.flatMap((option) => typeof option.value === "string" ? [{
        id: option.value,
        model: option.value,
        displayName: option.name || option.value,
        description: text(option.description),
        isDefault: option.value === model.currentValue,
        hidden: false,
        supportedReasoningEfforts,
        defaultReasoningEffort: this.currentEffort(),
      }] : []);
    }
    if (this.thread && model && typeof model.currentValue === "string") this.thread.model = model.currentValue;
  }

  private effortOption() {
    return this.configOptions.find((option) => option.category === "thought_level" || option.id === "effort" || option.id === "reasoning_effort");
  }

  private currentEffort() {
    return mapReasoningEffort(this.effortOption()?.currentValue);
  }

  /** Puts the session in the agent's no-prompt mode, matching other Treer Agents. */
  private async enableFullAccess(session: Json) {
    if (!this.client || !this.sessionId) return;
    const modeOption = this.configOptions.find((option) => option.category === "mode");
    const configured = modeOption?.options?.map((option) => option.value).find((value) => FULL_ACCESS_MODES.includes(String(value)));
    try {
      if (modeOption && configured && modeOption.currentValue !== configured) {
        const result = await this.client.request<Json>("session/set_config_option", { sessionId: this.sessionId, configId: modeOption.id, value: configured });
        this.readSettings(result);
        return;
      }
      const modes = session.modes && typeof session.modes === "object" ? session.modes as Json : {};
      const available = Array.isArray(modes.availableModes) ? modes.availableModes as Json[] : [];
      const mode = available.map((entry) => text(entry.id)).find((id) => FULL_ACCESS_MODES.includes(id));
      if (!modeOption && mode && modes.currentModeId !== mode) {
        await this.client.request("session/set_mode", { sessionId: this.sessionId, modeId: mode });
      }
    } catch (error) {
      this.emit("log", `could not enable full access: ${error instanceof Error ? error.message : String(error)}`);
    }
  }
}
