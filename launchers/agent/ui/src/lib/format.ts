import type { HistoryItem } from "../types";

export function duration(ms: number): string {
  const seconds = Math.max(0, Math.round(ms / 1000));
  if (seconds >= 3600) return `${Math.floor(seconds / 3600)}h ${Math.floor((seconds % 3600) / 60)}m`;
  if (seconds >= 60) return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
  return `${seconds}s`;
}

export function compact(count: number): string {
  return count >= 1000 ? `${(count / 1000).toFixed(count >= 10_000 ? 0 : 1)}k` : String(count);
}

export type ToolState = "done" | "failed" | "running";
/** Adapters report free-form status strings; normalise them for the chip and the failure marker. */
export function toolState(status: string | null | undefined): ToolState {
  const value = (status ?? "").toLowerCase();
  if (/fail|error|declin|denied|reject/.test(value)) return "failed";
  if (/progress|running|pending|started/.test(value)) return "running";
  return "done";
}

export interface ActivityStats { commands: number; changes: number; tools: number; steps: number; failed: number }
export function activityStats(items: HistoryItem[]): ActivityStats {
  const stats: ActivityStats = { commands: 0, changes: 0, tools: 0, steps: 0, failed: 0 };
  for (const item of items) {
    if (item.kind === "commandExecution") stats.commands++;
    else if (item.kind === "fileChange") stats.changes++;
    else if (item.kind === "reasoning") stats.steps++;
    else stats.tools++;
    if (toolState(item.status) === "failed") stats.failed++;
  }
  return stats;
}
