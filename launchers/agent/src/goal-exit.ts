import { randomUUID } from "node:crypto";
import { readGoalState, writeGoalState } from "./goal-storage.js";
import type { GoalUpdate } from "./model.js";
import type { HarnessRuntime } from "./runtime.js";

/** Return to the calling tool before native local-command input is processed. */
export class GoalExitRequests {
  private pending: { id: string; objective: string; goalId?: string; input: GoalUpdate }[] = [];
  private running = false;
  private closed = false;
  private timer?: ReturnType<typeof setTimeout>;
  constructor(private file: string, private runtime: HarnessRuntime, private log: (message: string) => void) {
    const saved = readGoalState(file);
    if (saved !== undefined) {
      if (!Array.isArray(saved) || saved.length > 256 || saved.some((entry) => typeof entry?.id !== "string" || typeof entry?.objective !== "string" || !entry.input)) throw new Error("Invalid goal exit journal");
      this.pending = saved;
    }
    runtime.on("state", () => { void this.flush(); });
    runtime.on("user-goal-clear", () => { this.pending = []; this.save(); });
    queueMicrotask(() => { void this.flush(); });
  }
  enqueue(input: GoalUpdate) {
    const goal = this.runtime.snapshot().goal;
    if (!goal) throw new Error("No goal set");
    if (this.pending.length >= 256) throw new Error("Goal exit queue is full");
    const entry = { id: randomUUID(), objective: goal.objective, goalId: goal.id, input };
    this.pending.push(entry); this.save();
    // Start only after the 202 response lets the Bash tool return.
    this.timer = setTimeout(() => { void this.flush(); }, 10); this.timer.unref();
    return { id: entry.id, accepted: true, status: "pending", note: "Native clear acknowledgement is required before goal state changes" };
  }
  close() { this.closed = true; if (this.timer) clearTimeout(this.timer); }
  private save() {
    writeGoalState(this.file, this.pending);
  }
  async flush() {
    if (this.running || this.closed || !this.runtime.snapshot().ready || !this.runtime.updateGoal) return;
    this.running = true;
    try {
      while (this.pending.length && !this.closed) {
        const entry = this.pending[0], goal = this.runtime.snapshot().goal;
        if (!goal || goal.objective !== entry.objective || entry.goalId && goal.id !== entry.goalId) { this.log("Discarded goal exit for a replaced goal"); this.pending.shift(); this.save(); continue; }
        try {
          if (goal.status !== entry.input.status || goal.reason !== entry.input.reason) await this.runtime.updateGoal(entry.input);
          if (this.pending[0] === entry) { this.pending.shift(); this.save(); }
        } catch (error) {
          if (!this.pending.includes(entry)) break;
          if (this.runtime.snapshot().goal?.syncStatus === "exit_unconfirmed") {
            this.log(`Goal exit unconfirmed; user must manually /goal clear: ${String(error)}`);
            this.pending.shift(); this.save(); continue;
          }
          this.log(`Goal exit pending; retrying native clear: ${String(error)}`);
          this.timer = setTimeout(() => { void this.flush(); }, 1000); this.timer.unref(); break;
        }
      }
    } finally { this.running = false; }
  }
}
