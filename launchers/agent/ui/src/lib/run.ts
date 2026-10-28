import { t, type MessageKey } from "../i18n";
import type { HistoryItem, Snapshot, ThreadState, TurnDto } from "../types";
import { diffCounts } from "./diff";
import { toolState } from "./format";

export type RunKey = "starting" | "restarting" | "working" | "blocked" | "failed" | "idle" | "reconnecting";
export type Tone = "busy" | "ok" | "warn" | "bad" | "quiet";
const TONES: Record<RunKey, Tone> = { starting: "warn", restarting: "warn", reconnecting: "warn", working: "busy", blocked: "bad", failed: "bad", idle: "quiet" };

/** One worded status for the header chip, the tab title and the announcer. Colour always comes with this word. */
export function runStatus(snapshot: Snapshot | null, connected: boolean, thread: ThreadState | null): { key: RunKey; label: string; tone: Tone } {
  let key: RunKey;
  if (!connected) key = "reconnecting";
  else if (!snapshot) key = "starting";
  else if (snapshot.runtime.state === "blocked") key = "blocked";
  else if (snapshot.runtime.recovery?.state === "restarting") key = "restarting";
  else if (snapshot.runtime.state === "starting") key = "starting";
  else if (snapshot.runtime.state === "working") key = "working";
  else key = thread?.turns.at(-1)?.status === "failed" ? "failed" : "idle";
  return { key, label: t(`status.${key}` as MessageKey), tone: TONES[key] };
}

/** What the live turn is doing right now, for the run bar. */
export function liveStep(turn: TurnDto | undefined): string {
  if (!turn) return "";
  for (let index = turn.items.length - 1; index >= 0; index--) {
    const item = turn.items[index];
    if (!item) continue;
    if (toolState(item.status) === "running") {
      if (item.kind === "commandExecution") return t("run.step.command", { command: firstLine(item.text) });
      if (item.kind === "toolCall") return t("run.step.tool", { tool: firstLine(item.text) });
      if (item.kind === "fileChange") return t("run.step.editing");
    }
    if (item.kind === "agentMessage") return t("run.step.writing");
    if (item.kind === "reasoning") return t("run.step.thinking");
    if (item.kind === "userMessage") continue; // a steer or the opening message says nothing about the current step
  }
  return t("run.step.thinking");
}
const firstLine = (text: string) => { const line = text.split("\n", 1)[0] ?? ""; return line.length > 80 ? `${line.slice(0, 79)}…` : line; };

export interface ChangedFile { path: string; kind: string; added: number; removed: number; diffs: string[] }
/** Every file a turn changed, merged across its change events, in first-touched order. */
export function changedFiles(items: HistoryItem[]): ChangedFile[] {
  const files = new Map<string, ChangedFile>();
  for (const item of items) {
    if (item.kind !== "fileChange") continue;
    for (const change of item.changes ?? []) {
      const counts = diffCounts(change.diff || "");
      const file = files.get(change.path) ?? { path: change.path, kind: change.kind, added: 0, removed: 0, diffs: [] };
      file.added += counts.added; file.removed += counts.removed; file.diffs.push(change.diff || "");
      if (file.kind !== change.kind && file.kind !== "add") file.kind = change.kind;
      files.set(change.path, file);
    }
  }
  return [...files.values()];
}

/** The message a person typed to start this turn, when it can be sent again as-is (no attachments, not relayed from another agent). */
export const RELAYED = /^\[来自 ([^\r\n]+)，排队于 ([^\r\n]+)\]\n/;
export function retryText(turn: TurnDto): string | null {
  const first = turn.items.find((item) => item.kind === "userMessage");
  if (!first || first.attachments?.length || RELAYED.test(first.text) || !first.text.trim()) return null;
  return first.text;
}

/** Short workspace label: the last two path segments. */
export function shortPath(path: string): string {
  const parts = path.replaceAll("\\", "/").split("/").filter(Boolean);
  return parts.length > 2 ? `…/${parts.slice(-2).join("/")}` : path;
}

export function latestPlan(turns: TurnDto[]): HistoryItem | null {
  for (let turn = turns.length - 1; turn >= 0; turn--) {
    const items = turns[turn]?.items ?? [];
    for (let index = items.length - 1; index >= 0; index--) if (items[index]?.kind === "plan" && items[index]?.steps?.length) return items[index] ?? null;
  }
  return null;
}
