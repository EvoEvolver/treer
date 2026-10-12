import type { Goal, GoalUpdate } from "./model.js";

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
    if (!["active", "paused", "blocked", "usageLimited", "budgetLimited", "complete"].includes(String(body.status))) throw new GoalError("Invalid goal status");
    result.status = body.status as Goal["status"];
  }
  if (body.tokenBudget !== undefined) {
    if (body.tokenBudget !== null && (typeof body.tokenBudget !== "number" || !Number.isSafeInteger(body.tokenBudget) || body.tokenBudget <= 0)) throw new GoalError("tokenBudget must be a positive integer or null");
    result.tokenBudget = body.tokenBudget as number | null;
  }
  if (!Object.keys(result).length) throw new GoalError("Provide objective, status, or tokenBudget");
  return result;
}
export function mapGoal(value: unknown): Goal | null {
  if (!value || typeof value !== "object") return null;
  const goal = value as Record<string, unknown>;
  if (typeof goal.objective !== "string") return null;
  const number = (value: unknown) => typeof value === "number" && Number.isFinite(value) ? value : null;
  const status = ["active", "paused", "blocked", "usageLimited", "budgetLimited", "complete"].includes(String(goal.status)) ? goal.status as Goal["status"] : "blocked";
  return { objective: goal.objective, status, tokenBudget: number(goal.tokenBudget), tokensUsed: number(goal.tokensUsed), timeUsedSeconds: number(goal.timeUsedSeconds) };
}
