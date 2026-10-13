import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { randomUUID } from "node:crypto";
import { readGoalState, writeGoalState } from "./goal-storage.js";
import type { Goal } from "./model.js";
import { sendNotification } from "./prompt-notify.js";

export async function goalRecipients(): Promise<string[]> {
  if (process.env.AGENT_UI_GOAL_NOTIFY_TO) return [process.env.AGENT_UI_GOAL_NOTIFY_TO];
  const { stdout } = await promisify(execFile)("treer", ["member", "list"], { timeout: 15000, maxBuffer: 65536 });
  const response = JSON.parse(stdout) as { humans: { role: string; user_id: string }[] };
  const members = response.humans;
  const owners = members.filter((member) => member.role === "owner").map((member) => member.user_id);
  if (!owners.length) throw new Error("No workspace owner available; set AGENT_UI_GOAL_NOTIFY_TO");
  return owners;
}

/** Persist before sending; retry using immutable sender-scoped idempotency keys. */
export class GoalNotifications {
  private pending: { id: string; goal: Goal; recipients?: string[] }[] = [];
  private running = false;
  private timer?: ReturnType<typeof setTimeout>;
  private closed = false;
  constructor(private file: string, private log: (message: string) => void,
    private send = sendNotification, private recipients = goalRecipients, private retryMs = 1000) {
    const saved = readGoalState(file);
    if (saved !== undefined) {
      if (!Array.isArray(saved) || saved.length > 256 || saved.some((entry) => typeof entry?.id !== "string" || !entry.goal || entry.recipients !== undefined && (!Array.isArray(entry.recipients) || entry.recipients.some((to: unknown) => typeof to !== "string")))) throw new Error("Invalid goal notification journal");
      this.pending = saved;
    }
    queueMicrotask(() => { void this.flush(); });
  }
  enqueue(goal: Goal) { if (this.pending.length >= 256) throw new Error("Goal notification outbox is full"); this.pending.push({ id: randomUUID(), goal: structuredClone(goal) }); this.save(); void this.flush(); }
  close() { this.closed = true; if (this.timer) clearTimeout(this.timer); }
  private save() {
    writeGoalState(this.file, this.pending);
  }
  async flush() {
    if (this.running || this.closed) return;
    this.running = true;
    try {
      while (this.pending.length && !this.closed) {
        const entry = this.pending[0];
        try {
          entry.recipients ??= await this.recipients(); this.save();
          const body = JSON.stringify({ type: "treer.goal.exit", agent_id: process.env.TREER_AGENT_ID || null,
            objective: entry.goal.objective, status: entry.goal.status, reason: entry.goal.reason,
            required_action: entry.goal.requiredAction || null });
          for (const recipient of entry.recipients) await this.send(recipient, `goal-exit/${entry.id}`, body);
          this.pending.shift(); this.save();
        } catch (error) {
          this.log(`Goal notification failed; retained for retry: ${error instanceof Error ? error.message : "unknown error"}`);
          this.timer = setTimeout(() => { void this.flush(); }, this.retryMs); this.timer.unref(); break;
        }
      }
    } finally { this.running = false; }
  }
}
