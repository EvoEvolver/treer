import { randomUUID } from "node:crypto";
import type { Goal, GoalUpdate } from "./model.js";
import type { HarnessRuntime } from "./runtime.js";
import type { PromptQueue } from "./prompt-queue.js";
import { readGoalState, writeGoalState } from "./goal-storage.js";
import { GoalError } from "./goals.js";

interface Edit { id: string; input: GoalUpdate; previous: Goal | null; goal: Goal | null; applied: boolean; status: "pending" | "sent" | "failed"; error?: string; promptId?: string; mode?: "steer" | "queue"; attempt?: number }
export function goalEditPrompt(previous: Goal | null, goal: Goal | null) {
  return `The user changed the goal. New objective (user-authored): ${goal?.objective || "None"}\nPrevious objective: ${previous?.objective || "None"}\nStatus: ${goal?.status || "cleared"}. Token budget: ${goal?.tokenBudget ?? "default"}. Round budget: ${goal?.roundBudget ?? "default"}.\n${goal?.status === "paused" ? "Pause goal-directed work and await the user's instructions." : "Use the updated goal and budget; do not keep following the previous objective."}`;
}

/** Native mutations and model-context delivery have separate durable receipts. */
export class GoalEdits {
  private edits: Edit[] = [];
  private running = false;
  private closed = false;
  private timer?: ReturnType<typeof setTimeout>;
  constructor(private file: string | undefined, private runtime: HarnessRuntime, private queue: PromptQueue, private changed: () => void) {
    const saved = file && readGoalState(file);
    if (saved) {
      if (!Array.isArray(saved) || saved.length > 1024 || saved.some((edit) => typeof edit.id !== "string" || !edit.input || !["pending", "sent", "failed"].includes(edit.status))) throw new Error("Invalid goal edit journal");
      this.edits = saved;
    }
    runtime.on("state", () => { void this.flush(); });
  }
  get state() { const edit = this.edits.at(-1); return edit ? { id: edit.id, status: edit.status, error: edit.error || null, promptId: edit.promptId || null } : null; }
  private save() { if (this.file) writeGoalState(this.file, this.edits); this.changed(); }
  async apply(input: GoalUpdate, id: string = randomUUID()) {
    if (!/^[A-Za-z0-9_-]{1,256}$/.test(id)) throw new GoalError("Invalid goal edit operationId");
    let edit = this.edits.find((entry) => entry.id === id);
    if (edit && JSON.stringify(edit.input) !== JSON.stringify(input)) throw new GoalError("Goal edit operationId was used with different input", 409);
    if (!edit) {
      if (this.edits.filter((entry) => entry.status !== "sent").length >= 256) throw new GoalError("Goal edit delivery queue is full", 429);
      edit = { id, input, previous: structuredClone(this.runtime.snapshot().goal), goal: null, applied: false, status: "pending" };
      this.edits = this.edits.filter((entry) => entry.status !== "sent" || this.edits.indexOf(entry) >= this.edits.length - 768);
      this.edits.push(edit); this.save();
    }
    await this.flush();
    if (!edit.applied && edit.error) throw new GoalError(edit.error, 503);
    return this.state;
  }
  retry(id: string) {
    const edit = this.edits.find((entry) => entry.id === id);
    if (!edit) throw new GoalError("Unknown goal edit", 404);
    if (edit.status !== "sent") { edit.status = "pending"; edit.error = undefined; this.save(); void this.flush(); }
  }
  close() { this.closed = true; if (this.timer) clearTimeout(this.timer); }
  async flush() {
    if (this.running || this.closed || !this.runtime.snapshot().ready) return;
    this.running = true;
    try {
      for (const edit of this.edits.filter((entry) => entry.status !== "sent")) {
        try {
          if (!edit.applied) {
            if (!this.runtime.updateGoal) throw new GoalError("Goals unsupported", 501);
            await this.runtime.updateGoal(edit.input);
            edit.applied = true; edit.goal = structuredClone(this.runtime.snapshot().goal); this.save();
          }
          edit.mode ??= this.queue.busy ? "steer" : "queue"; this.save();
          const result = await this.queue.submit(goalEditPrompt(edit.previous, edit.goal), edit.mode, { operationId: `goal-edit/${edit.id}/${edit.attempt ?? 0}`, notify: { on: [] } });
          const submission = this.queue.submission(result.id);
          if (submission?.state === "failed" || submission?.state === "interrupted") {
            edit.attempt = (edit.attempt ?? 0) + 1; edit.mode = undefined;
            throw new Error(submission.error || "Goal edit prompt failed before confirmed delivery");
          }
          edit.promptId = result.id; edit.status = "sent"; edit.error = undefined; this.save();
        } catch (error) {
          edit.status = "failed"; edit.error = error instanceof Error ? error.message : "Goal edit delivery failed"; this.save();
          this.timer = setTimeout(() => { void this.flush(); }, 1000); this.timer.unref(); break;
        }
      }
    } finally { this.running = false; }
  }
}
