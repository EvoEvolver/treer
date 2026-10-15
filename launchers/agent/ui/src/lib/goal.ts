import type { Goal, Snapshot } from "../types";

export const GOAL_STATUSES = ["active", "paused", "blocked", "stale", "done", "cleared", "usageLimited", "budgetLimited", "complete"] as const;
export type GoalStatus = (typeof GOAL_STATUSES)[number];
const ATTENTION = new Set<GoalStatus>(["blocked", "stale", "usageLimited", "budgetLimited"]);

/** A cleared goal is gone as far as the reader is concerned; the strip offers "Set goal" again. */
export function currentGoal(snapshot: Snapshot): Goal | null {
  return snapshot.goal && snapshot.goal.status !== "cleared" ? snapshot.goal : null;
}

/** Unknown statuses from a newer adapter are shown as blocked, as the current UI does. */
export function goalStatus(goal: Goal): GoalStatus {
  return (GOAL_STATUSES as readonly string[]).includes(goal.status) ? goal.status : "blocked";
}
/** A goal needs the reader's attention when it cannot make progress or its native copy drifted. */
export function needsAttention(snapshot: Snapshot): boolean {
  const goal = currentGoal(snapshot);
  if (!goal) return false;
  return ATTENTION.has(goalStatus(goal)) || Boolean(goal.syncStatus && goal.syncStatus !== "synced") || snapshot.goalDelivery?.status === "failed";
}
export function formatDuration(seconds: number): string {
  return `${Math.floor(seconds / 60)}m ${Math.floor(seconds % 60)}s`;
}
