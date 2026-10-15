// Wire types of the launcher interface, mirrored by hand from src/model.ts and src/runtime.ts. Every field the UI
// does not rely on is optional or omitted. Optional fields added for the UI (usage, endedAt, parentId) may be absent.

export type ReasoningEffort = "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra";

export interface HistoryItem {
  id: string;
  kind: "userMessage" | "agentMessage" | "reasoning" | "commandExecution" | "fileChange" | "plan" | "toolCall" | "other";
  text: string;
  previewText?: string;
  detailText?: string;
  status?: string | null;
  changes?: Array<{ path: string; diff: string; kind: string }>;
  steps?: Array<{ step: string; status: "pending" | "inProgress" | "completed" }>;
  startedAt?: string | null;
  endedAt?: string | null;
  parentId?: string | null;
}

export interface TurnDto {
  id: string;
  startedAt: string | null;
  status: "completed" | "interrupted" | "failed" | "inProgress";
  error: string | null;
  items: HistoryItem[];
  endedAt?: string | null;
  usage?: { inputTokens?: number; outputTokens?: number; cachedTokens?: number; durationMs?: number };
}

export interface ModelOption {
  id: string;
  model: string;
  displayName: string;
  description: string;
  isDefault: boolean;
  hidden: boolean;
  supportedReasoningEfforts: Array<{ reasoningEffort: ReasoningEffort; description: string }>;
  defaultReasoningEffort: ReasoningEffort | null;
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

export interface QueuedPrompt {
  id: string;
  text: string;
  createdAt: string;
  sender?: { agent_id: string; path: string; name: string } | null;
  position?: number;
}

export interface BackgroundTask {
  id: string;
  kind: "shell" | "agent" | "monitor" | "other";
  description: string;
  status: "running" | "completed" | "failed" | "stopped";
  startedAt: string | null;
  endedAt: string | null;
  exitCode: number | null;
  summary: string | null;
}

export interface Goal {
  id?: string;
  objective: string;
  status: "active" | "paused" | "blocked" | "stale" | "done" | "cleared" | "usageLimited" | "budgetLimited" | "complete";
  tokenBudget: number | null;
  tokensUsed: number | null;
  timeUsedSeconds: number | null;
  reason?: string;
  requiredAction?: string;
  syncStatus?: "synced" | "desynced" | "exit_unconfirmed";
  syncError?: string;
}

export interface HarnessInfo {
  id: string;
  name: string;
  capabilities: { steer: boolean; backgroundTasks: boolean; goal: { pause: boolean; budget: boolean } | null };
}

export type RuntimeState = "starting" | "working" | "blocked" | "idle";

/** The body of GET api/state and of every WebSocket `state` frame (the frame adds `type`). */
export interface Snapshot {
  type?: "state";
  ready: boolean;
  runtime: { state: RuntimeState; error: string | null };
  harness: HarnessInfo;
  thread: ThreadState | null;
  modelOptions: ModelOption[];
  backgroundTasks: BackgroundTask[];
  queue: QueuedPrompt[];
  goal: Goal | null;
  history: { limit: number };
  submission?: { downgraded?: boolean };
}

export type PromptMode = "steer" | "queue";
