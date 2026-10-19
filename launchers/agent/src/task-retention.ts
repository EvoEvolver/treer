import type { BackgroundTask } from "./model.js";

/** Retention for background-task records (#184): running tasks are never pruned; finished ones are bounded by count and age. */
export const MAX_FINISHED_TASKS = 20;
export const MAX_FINISHED_TASK_AGE_MS = 24 * 60 * 60 * 1000;

export const isFinished = (task: BackgroundTask) => task.status !== "running";

/**
 * Removes finished records beyond the newest `maxFinished` and any finished more than `maxAgeMs` ago. Touches only the
 * launcher's record map: it never stops a task and never reads or deletes task output. Returns how many were removed.
 */
export function pruneFinishedTasks(tasks: Map<string, BackgroundTask>, now = Date.now(), maxFinished = MAX_FINISHED_TASKS, maxAgeMs = MAX_FINISHED_TASK_AGE_MS): number {
  const finished = [...tasks.values()].filter(isFinished).map((task, order) => ({ task, order }));
  if (!finished.length) return 0;
  const endedAt = (task: BackgroundTask) => { const value = task.endedAt ? Date.parse(task.endedAt) : NaN; return Number.isFinite(value) ? value : 0; };
  // Newest first. Equal end times (the launcher stamps them to the millisecond) fall back to arrival order, later = newer;
  // a missing or invalid end time counts as oldest.
  finished.sort((a, b) => endedAt(b.task) - endedAt(a.task) || b.order - a.order);
  let removed = 0;
  finished.forEach(({ task }, index) => {
    if (index >= maxFinished || now - endedAt(task) > maxAgeMs) { tasks.delete(task.id); removed++; }
  });
  return removed;
}

/** Removes every finished record now (the user's "Clear finished tasks"); running tasks stay. */
export function clearFinishedTasks(tasks: Map<string, BackgroundTask>): number {
  let removed = 0;
  for (const task of [...tasks.values()]) if (isFinished(task)) { tasks.delete(task.id); removed++; }
  return removed;
}
