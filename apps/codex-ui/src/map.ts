export interface HistoryItem {
  id: string;
  kind:
    | "userMessage"
    | "agentMessage"
    | "reasoning"
    | "commandExecution"
    | "fileChange"
    | "toolCall"
    | "other";
  text: string;
  previewText?: string;
  detailText?: string;
  status?: string | null;
  transcriptOrder?: number | null;
  sourceTurnId?: string | null;
}

export interface TurnDto {
  id: string;
  startedAt: string | null;
  status: "completed" | "interrupted" | "failed" | "inProgress";
  error: string | null;
  items: HistoryItem[];
}

interface CodexItem {
  type?: string;
  id?: string;
  text?: string;
  content?: Array<{ type?: string; text?: string }>;
  summary?: string[];
  command?: string;
  aggregatedOutput?: string | null;
  status?: string | null;
  [key: string]: unknown;
}

interface CodexTurn {
  id?: string;
  status?: string;
  error?: { message?: string } | null;
  items?: CodexItem[];
}

function itemText(item: CodexItem) {
  const content = Array.isArray(item.content)
    ? item.content
        .map((entry) => (typeof entry.text === "string" ? entry.text : ""))
        .filter(Boolean)
        .join("\n")
    : "";
  if (typeof item.text === "string" && item.text.trim()) {
    return content && content.includes("\n") ? content : item.text;
  }
  return content;
}

export function mapItem(item: CodexItem, turnId: string, order: number): HistoryItem {
  const id = typeof item.id === "string" ? item.id : `item-${order}`;
  const type = item.type ?? "other";
  const text = itemText(item);
  const base = { id, transcriptOrder: order, sourceTurnId: turnId };
  switch (type) {
    case "userMessage":
      return { ...base, kind: "userMessage", text };
    case "agentMessage":
    case "text":
      return { ...base, kind: "agentMessage", text };
    case "reasoning":
      return {
        ...base,
        kind: "reasoning",
        text: [item.summary?.join("\n") ?? "", text].filter(Boolean).join("\n\n"),
      };
    case "commandExecution":
      return {
        ...base,
        kind: "commandExecution",
        text: typeof item.command === "string" ? item.command : text,
        previewText: text.slice(0, 160) || (item.command as string | undefined),
        detailText: typeof item.aggregatedOutput === "string" ? item.aggregatedOutput : text,
        status: item.status ?? null,
      };
    case "fileChange":
    case "file_change":
      return { ...base, kind: "fileChange", text: text || "File change", status: item.status ?? null };
    default:
      return { ...base, kind: type.toLowerCase().includes("tool") ? "toolCall" : "other", text: text || type };
  }
}

export function mapTurn(turn: CodexTurn): TurnDto {
  const id = typeof turn.id === "string" ? turn.id : "turn";
  const status = turn.status === "interrupted" || turn.status === "failed" || turn.status === "inProgress"
    ? turn.status
    : "completed";
  return {
    id,
    startedAt: null,
    status,
    error: turn.error?.message ?? null,
    items: (turn.items ?? []).map((item, index) => mapItem(item, id, index)),
  };
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

function stringField(record: Record<string, unknown>, ...keys: string[]) {
  for (const key of keys) {
    const value = record[key];
    if (typeof value === "string" && value.trim()) {
      return value.trim();
    }
  }
  return "";
}

function boolField(record: Record<string, unknown>, ...keys: string[]) {
  for (const key of keys) {
    if (typeof record[key] === "boolean") {
      return record[key] as boolean;
    }
  }
  return false;
}

export function mapModelOption(raw: unknown, index: number): ModelOption | null {
  if (!raw || typeof raw !== "object") {
    return null;
  }
  const record = raw as Record<string, unknown>;
  const model = stringField(record, "model", "id");
  if (!model) {
    return null;
  }
  const effortsRaw = record.supportedReasoningEfforts ?? record.supported_reasoning_efforts;
  const supportedReasoningEfforts = Array.isArray(effortsRaw)
    ? effortsRaw.flatMap((entry) => {
        if (!entry || typeof entry !== "object") {
          const effort = mapReasoningEffort(entry);
          return effort ? [{ reasoningEffort: effort, description: "" }] : [];
        }
        const item = entry as Record<string, unknown>;
        const effort = mapReasoningEffort(item.reasoningEffort ?? item.reasoning_effort ?? item.effort);
        return effort
          ? [{ reasoningEffort: effort, description: stringField(item, "description") }]
          : [];
      })
    : [];
  const defaultReasoningEffort = mapReasoningEffort(
    record.defaultReasoningEffort ?? record.default_reasoning_effort,
  );
  return {
    id: stringField(record, "id") || `${model}-${index}`,
    model,
    displayName: stringField(record, "displayName", "display_name") || model,
    description: stringField(record, "description"),
    isDefault: boolField(record, "isDefault", "is_default"),
    hidden: boolField(record, "hidden"),
    supportedReasoningEfforts,
    defaultReasoningEffort: defaultReasoningEffort ?? supportedReasoningEfforts[0]?.reasoningEffort ?? null,
  };
}

export function fallbackModelOption(model: string | null, effort: ReasoningEffort | null): ModelOption[] {
  if (!model) {
    return [];
  }
  const supportedReasoningEfforts: ModelOption["supportedReasoningEfforts"] = [
    { reasoningEffort: "low", description: "" },
    { reasoningEffort: "medium", description: "" },
    { reasoningEffort: "high", description: "" },
    { reasoningEffort: "xhigh", description: "" },
  ];
  const defaultReasoningEffort =
    effort && supportedReasoningEfforts.some((entry) => entry.reasoningEffort === effort)
      ? effort
      : "medium";
  return [
    {
      id: model,
      model,
      displayName: model,
      description: "",
      isDefault: true,
      hidden: false,
      supportedReasoningEfforts,
      defaultReasoningEffort,
    },
  ];
}

export function yoloResponse(method: string, params: unknown) {
  switch (method) {
    case "item/commandExecution/requestApproval":
    case "item/fileChange/requestApproval":
      return { decision: "accept" };
    case "item/permissions/requestApproval": {
      const permissions =
        params && typeof params === "object" && "permissions" in params
          ? (params as { permissions?: unknown }).permissions
          : {};
      return { permissions: permissions ?? {}, scope: "turn" };
    }
    case "execCommandApproval":
    case "applyPatchApproval":
      return { decision: "approved" };
    default:
      return { decision: "accept" };
  }
}
