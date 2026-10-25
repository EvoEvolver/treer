import { t, type MessageKey } from "../i18n";
import { currentGoal, goalStatus, needsAttention } from "../lib/goal";
import { useStore } from "../state/store";
import type { HistoryItem, TurnDto } from "../types";
import { Icon } from "./Icon";

export type PopName = "goal" | "plan" | "queue" | "model" | "actions";

export function latestPlan(turns: TurnDto[]): HistoryItem | null {
  for (let t = turns.length - 1; t >= 0; t--) {
    const items = turns[t]?.items ?? [];
    for (let i = items.length - 1; i >= 0; i--) if (items[i]?.kind === "plan" && items[i]?.steps?.length) return items[i] ?? null;
  }
  return null;
}

/** Goal, plan progress and queue count as one 32px row. One popover at a time; the goal segment turns warning-coloured when it needs attention. */
export function Strip({ pop, onToggle }: { pop: PopName | null; onToggle: (name: PopName, trigger: HTMLElement) => void }) {
  const { state } = useStore();
  const snapshot = state.snapshot;
  if (!snapshot) return null;
  const capability = snapshot.harness.capabilities.goal;
  const plan = latestPlan(snapshot.thread?.turns ?? []);
  const done = plan?.steps?.filter((step) => step.status === "completed").length ?? 0;
  const queued = snapshot.queue.length;
  if (!(capability && currentGoal(snapshot)) && !plan && !queued) return null;
  const goal = currentGoal(snapshot);
  const attention = needsAttention(snapshot);
  const label = goal ? t(`goal.status.${goalStatus(goal)}` as MessageKey) : "";
  return (
    <div className="strip" role="toolbar" aria-label={t("strip.label")}>
      {capability && goal && (
        <button type="button" className={attention ? "seg goal attn" : "seg goal"} aria-expanded={pop === "goal"} aria-controls="pop-goal" onClick={(event) => onToggle("goal", event.currentTarget)}>
          <Icon name={attention ? "warn" : "goal"} /><b>{t("goal.title")}</b><span className="tx">{attention || goalStatus(goal) !== "active" ? `${label} · ${goal.objective}` : goal.objective}</span>
        </button>
      )}
      {plan && <button type="button" className="seg" aria-expanded={pop === "plan"} aria-controls="pop-plan" onClick={(event) => onToggle("plan", event.currentTarget)}><Icon name="plan" /><b>{t("plan.segment")}</b><span>{done}/{plan.steps?.length ?? 0}</span></button>}
      {queued > 0 && <button type="button" className="seg" aria-expanded={pop === "queue"} aria-controls="pop-queue" onClick={(event) => onToggle("queue", event.currentTarget)}><Icon name="queue" /><b>{t("queue.segment")}</b><span>{queued}</span></button>}
    </div>
  );
}
