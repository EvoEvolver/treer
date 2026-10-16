// Harness-neutral history and settings shared by the server, frontend contract,
// and every harness adapter.

/** Public metadata only; bytes and local paths never enter browser snapshots. */
export interface ImageAttachment {
  id: string;
  mime: "image/png" | "image/jpeg" | "image/webp" | "image/gif";
  size: number;
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
  outputFile?: string;
}

export interface QueuedPrompt {
  id: string;
  text: string;
  createdAt: string;
  sender?: { agent_id: string; path: string; name: string } | null;
  position?: number;
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
  roundBudget?: number | null;
  roundsUsed?: number;
  rejectionCount?: number;
  consecutiveRejections?: number;
  notificationError?: string;
  syncStatus?: "synced" | "desynced" | "exit_unconfirmed";
  syncError?: string;
  nativeObjective?: string;
}

export interface GoalUpdate {
  objective?: string;
  status?: Goal["status"];
  tokenBudget?: number | null;
  roundBudget?: number | null;
  reason?: string;
  requiredAction?: string;
}

export interface HistoryItem {
  id: string;
  /** Adapter-only reconciliation keys; the server removes these from snapshots. */
  attachmentDigests?: string[];
  attachments?: ImageAttachment[];
  kind:
    | "userMessage"
    | "agentMessage"
    | "reasoning"
    | "commandExecution"
    | "fileChange"
    | "plan"
    | "toolCall"
    | "other";
  text: string;
  previewText?: string;
  detailText?: string;
  status?: string | null;
  transcriptOrder?: number | null;
  sourceTurnId?: string | null;
  changes?: Array<{ path: string; diff: string; kind: string }>;
  steps?: Array<{ step: string; status: "pending" | "inProgress" | "completed" }>;
}

export interface TurnDto {
  id: string;
  startedAt: string | null;
  status: "completed" | "interrupted" | "failed" | "inProgress";
  error: string | null;
  items: HistoryItem[];
}

export type ReasoningEffort =
  | "none"
  | "minimal"
  | "low"
  | "medium"
  | "high"
  | "xhigh"
  | "max"
  | "ultra";

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

const REASONING_EFFORTS = new Set<ReasoningEffort>([
  "none",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
  "ultra",
]);

export function mapReasoningEffort(value: unknown): ReasoningEffort | null {
  if (typeof value !== "string") {
    return null;
  }
  const effort = value.trim().toLowerCase() as ReasoningEffort;
  return REASONING_EFFORTS.has(effort) ? effort : null;
}

export function mapPlanSteps(raw: unknown): NonNullable<HistoryItem["steps"]> {
  const entries: unknown[] = typeof raw === "string" ? raw.split("\n").filter((line) => line.trim()) : Array.isArray(raw) ? raw : [];
  return entries.flatMap((entry) => {
    const record = typeof entry === "string" ? { step: entry, status: /^\s*[-*]?\s*\[x\]/i.test(entry) ? "completed" : "pending" } : entry && typeof entry === "object" ? entry as Record<string, unknown> : {};
    const step = record.step ?? record.text ?? record.title ?? record.content;
    if (typeof step !== "string" || !step.trim()) return [];
    const status = record.status === "completed" || record.status === "done" ? "completed" : ["inProgress", "in_progress", "running"].includes(String(record.status)) ? "inProgress" : "pending";
    return [{ step: step.replace(/^\s*(?:[-*]|\d+\.)?\s*(?:\[[ xX]\]\s*)?/, "").trim(), status }];
  });
}
