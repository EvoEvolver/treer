import type { Goal, GoalUpdate } from "./model.js";
import { createHash } from "node:crypto";

export class GoalError extends Error {
  constructor(message: string, readonly status = 400) { super(message); }
}
export function goalUpdate(body: Record<string, unknown>): GoalUpdate {
  const result: GoalUpdate = {};
  if (body.objective !== undefined) {
    if (typeof body.objective !== "string" || !body.objective.trim()) throw new GoalError("objective must not be empty");
    result.objective = body.objective.trim();
  }
  if (body.status !== undefined) {
    if (!["active", "paused", "blocked", "stale", "done", "cleared", "usageLimited", "budgetLimited", "complete"].includes(String(body.status))) throw new GoalError("Invalid goal status");
    result.status = body.status as Goal["status"];
  }
  if (body.tokenBudget !== undefined) {
    if (body.tokenBudget !== null && (typeof body.tokenBudget !== "number" || !Number.isSafeInteger(body.tokenBudget) || body.tokenBudget <= 0)) throw new GoalError("tokenBudget must be a positive integer or null");
    result.tokenBudget = body.tokenBudget as number | null;
  }
  for (const key of ["reason", "requiredAction"] as const) {
    if (body[key] !== undefined) {
      if (typeof body[key] !== "string" || !body[key].trim() || Buffer.byteLength(body[key]) > 4096) throw new GoalError(`${key} must contain text within 4 KiB`);
      result[key] = body[key].trim();
    }
  }
  if (body.roundBudget !== undefined) {
    if (body.roundBudget !== null && (!Number.isSafeInteger(body.roundBudget) || Number(body.roundBudget) < 1 || Number(body.roundBudget) > 10000)) throw new GoalError("roundBudget must be an integer from 1 to 10000 or null");
    result.roundBudget = body.roundBudget === null ? null : Number(body.roundBudget);
  }
  if (["blocked", "stale", "done"].includes(result.status || "") && !result.reason) throw new GoalError("A goal exit requires reason");
  if (!Object.keys(result).length) throw new GoalError("Provide objective, status, or tokenBudget");
  return result;
}
export function mapGoal(value: unknown): Goal | null {
  if (!value || typeof value !== "object") return null;
  const goal = value as Record<string, unknown>;
  if (typeof goal.objective !== "string") return null;
  const number = (value: unknown) => typeof value === "number" && Number.isFinite(value) ? value : null;
  const status = goal.status === "complete" ? "done" : ["active", "paused", "blocked", "stale", "done", "cleared", "usageLimited", "budgetLimited"].includes(String(goal.status)) ? goal.status as Goal["status"] : "blocked";
  return { objective: goal.objective, status, tokenBudget: number(goal.tokenBudget), tokensUsed: number(goal.tokensUsed), timeUsedSeconds: number(goal.timeUsedSeconds) };
}

export class GoalGuard {
  private evidence = new Set<string>();
  private revision = 0;
  private lastRevision = 0;
  rejections = 0;
  totalRejections = 0;
  rounds = 0;
  tokens = 0;
  constructor(readonly rejectionLimit = positiveSetting("AGENT_UI_GOAL_NO_PROGRESS_LIMIT", 3),
    readonly roundBudget: number | null = budgetSetting("AGENT_UI_GOAL_ROUND_BUDGET"),
    readonly tokenBudget: number | null = budgetSetting("AGENT_UI_GOAL_TOKEN_BUDGET")) {}
  progress(value: string) {
    const key = createHash("sha256").update(value).digest("hex");
    if (!this.evidence.has(key)) { this.evidence.add(key); this.revision++; }
  }
  reject() {
    this.rounds++;
    this.totalRejections++;
    this.rejections = this.revision === this.lastRevision ? this.rejections + 1 : 1;
    this.lastRevision = this.revision;
    return this.rejections >= this.rejectionLimit ? "Repeated Stop hook rejection without new tool or result evidence" : this.limit();
  }
  usage(tokens: number) { this.tokens += Math.max(0, tokens); return this.limit(); }
  limit() { return this.rejections >= this.rejectionLimit ? "Repeated Stop hook rejection without new tool or result evidence" : this.roundBudget !== null && this.rounds >= this.roundBudget ? "Goal round budget reached" : this.tokenBudget !== null && this.tokens >= this.tokenBudget ? "Goal token budget reached" : null; }
}
function budgetSetting(name: string) {
  const value = process.env[name];
  return value === undefined || value === "" || value === "0" ? null : positiveSetting(name, 0);
}
function positiveSetting(name: string, fallback: number) {
  const value = process.env[name];
  if (value === undefined) return fallback;
  if (!/^\d+$/.test(value) || !Number.isSafeInteger(Number(value)) || Number(value) < 1) throw new GoalError(`${name} must be a positive integer`);
  return Number(value);
}
