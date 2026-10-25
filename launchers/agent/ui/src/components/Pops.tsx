import { useEffect, useRef, type ReactNode } from "react";
import { t, type MessageKey } from "../i18n";
import { currentGoal, formatDuration, goalStatus } from "../lib/goal";
import { useStore } from "../state/store";
import { Icon } from "./Icon";
import { latestPlan } from "./Strip";

export function Pop({ name, title, closeLabel, count, onClose, onBack, children }: { name: string; title: string; closeLabel: string; count?: string; onClose: () => void; onBack?: () => void; children: ReactNode }) {
  const close = useRef<HTMLButtonElement>(null);
  useEffect(() => { close.current?.focus({ preventScroll: true }); }, []);
  return (
    <section className="pop" id={`pop-${name}`} role="region" aria-label={title}>
      <header className="pop-h">{onBack && <button type="button" className="xbtn" aria-label={t("composer.back")} onClick={onBack}><Icon name="back" /></button>}<h3>{title}{count && <span className="cnt"> {count}</span>}</h3><button type="button" ref={close} className="xbtn" aria-label={closeLabel} onClick={onClose}><Icon name="close" /></button></header>
      {children}
    </section>
  );
}

export function PlanPop({ onClose }: { onClose: () => void }) {
  const { state } = useStore();
  const plan = latestPlan(state.snapshot?.thread?.turns ?? []);
  const steps = plan?.steps ?? [];
  const icon = { completed: "check", inProgress: "now", pending: "todo" } as const;
  return (
    <Pop name="plan" title={t("plan.title")} count={`${steps.filter((step) => step.status === "completed").length}/${steps.length}`} closeLabel={t("plan.close")} onClose={onClose}>
      <ul className="plist">{steps.map((step, index) => <li key={index} className={step.status === "completed" ? "done" : step.status === "inProgress" ? "cur" : undefined}><Icon name={icon[step.status]} /><span>{step.step}</span><span className="sr-only">{t(`plan.${step.status}` as MessageKey)}</span></li>)}</ul>
    </Pop>
  );
}

export function QueuePop({ onClose }: { onClose: () => void }) {
  const { state, actions } = useStore();
  const queue = state.snapshot?.queue ?? [];
  return (
    <Pop name="queue" title={t("queue.title")} closeLabel={t("queue.close")} onClose={onClose}>
      {queue.map((entry) => (
        <div className="qrow" key={entry.id}>
          <span className="qn">{entry.position ?? ""}</span>
          <span className="qt">{entry.text}{entry.sender?.path && <small className="qfrom"> · {t("queue.from", { path: entry.sender.path })}</small>}</span>
          <button type="button" className="btn" aria-label={t("queue.cancelAria", { text: entry.text })} onClick={() => void actions.cancelQueued(entry.id)}>{t("queue.cancel")}</button>
        </div>
      ))}
    </Pop>
  );
}

export function GoalPop({ onBack, confirming, onClose, onEdit, onAskClear, onKeep, onCleared }: { onBack?: () => void; confirming: boolean; onClose: () => void; onEdit: () => void; onAskClear: () => void; onKeep: () => void; onCleared: () => void }) {
  const { state, actions, dispatch } = useStore();
  const snapshot = state.snapshot;
  const goal = snapshot ? currentGoal(snapshot) : null, capability = snapshot?.harness.capabilities.goal;
  if (!snapshot || !goal || !capability) return null;
  const status = goalStatus(goal);
  const busy = state.goalBusy;
  const report = (message: string | null) => { if (message) dispatch({ type: "error", message }); };
  const tokens = goal.tokensUsed == null ? t("goal.tokensUnavailable") : t("goal.tokensUsed", { n: Number(goal.tokensUsed).toLocaleString() });
  const time = goal.timeUsedSeconds == null ? t("goal.timeUnavailable") : t("goal.timeUsed", { time: formatDuration(goal.timeUsedSeconds) });
  const delivery = snapshot.goalDelivery;
  const budget = Number.isFinite(goal.tokenBudget) && (goal.tokenBudget ?? 0) > 0 ? goal.tokenBudget as number : null;
  return (
    <Pop onBack={onBack} name="goal" title={t("goal.title")} closeLabel={t("goal.close")} onClose={onClose}>
      <p className="goal-text">{goal.objective}</p>
      <p className="goal-status"><b>{t(`goal.status.${status}` as MessageKey)}</b></p>
      <p className="meta">{tokens} · {time}</p>
      {goal.reason && <p className="goal-note">{goal.reason}</p>}
      {goal.requiredAction && <p className="goal-note">{t("goal.requiredAction", { action: goal.requiredAction })}</p>}
      {goal.rejectionCount != null && <p className="goal-note">{t("goal.rejections", { count: goal.rejectionCount, streak: goal.consecutiveRejections ?? 0 })}</p>}
      {goal.syncStatus && goal.syncStatus !== "synced" && (
        <p className="goal-note goal-warn">{goal.syncStatus}: {goal.syncError || t("goal.syncNotAligned")}{goal.syncStatus === "desynced" && <> <button type="button" className="btn" disabled={busy} onClick={() => void actions.reconcileGoal()}>{t("goal.realign")}</button></>}</p>
      )}
      {delivery && (
        <p className="goal-note">{delivery.status === "sent" ? t("goal.delivery.sent") : delivery.status === "failed" ? t("goal.delivery.failed", { error: delivery.error || "unknown error" }) : t("goal.delivery.sending")}
          {delivery.status === "failed" && <> <button type="button" className="btn" disabled={busy} onClick={() => void actions.retryGoalDelivery(delivery.id)}>{t("goal.delivery.retry")}</button></>}</p>
      )}
      {budget != null && (
        <>
          <progress className="meter" aria-label={t("goal.budgetLabel")} max={budget} value={Math.min(budget, Math.max(0, goal.tokensUsed ?? 0))} />
          <p className="meta">{t("goal.budget", { used: goal.tokensUsed == null ? "—" : Number(goal.tokensUsed).toLocaleString(), budget: budget.toLocaleString() })}</p>
        </>
      )}
      {capability.budget && <p className="meta">{t("goal.budgetSummary", { tokens: budget == null ? t("goal.unlimited") : budget.toLocaleString(), rounds: goal.roundBudget == null ? t("goal.unlimited") : goal.roundBudget.toLocaleString() })}</p>}
      {confirming ? (
        <div className="goal-confirm" role="group" aria-label={t("goal.confirm")}>
          <span>{t("goal.confirmQuestion")}</span>
          <button type="button" className="btn danger" disabled={busy} onClick={async () => { const error = await actions.clearGoal(); if (error) report(error); else onCleared(); }} autoFocus>{t("goal.confirm")}</button>
          <button type="button" className="btn" onClick={onKeep}>{t("goal.keep")}</button>
        </div>
      ) : (
        <div className="pop-actions">
          <button type="button" className="btn" disabled={busy} onClick={onEdit}>{t("goal.edit")}</button>
          {capability.pause && (status === "active" || status === "paused") && (
            <button type="button" className="btn" disabled={busy} onClick={async () => report(await actions.putGoal({ status: status === "paused" ? "active" : "paused" }))}>{status === "paused" ? t("goal.resume") : t("goal.pause")}</button>
          )}
          <button type="button" className="btn" disabled={busy} onClick={onAskClear}>{t("goal.clear")}</button>
        </div>
      )}
    </Pop>
  );
}

export function ModelPop({ onClose, onBack }: { onClose: () => void; onBack?: () => void }) {
  const { state, actions } = useStore();
  const snapshot = state.snapshot;
  const thread = snapshot?.thread;
  const models = snapshot?.modelOptions ?? [];
  const selected = models.find((option) => option.model === thread?.model);
  const efforts = selected?.supportedReasoningEfforts ?? [];
  const busy = state.settingsBusy || !thread;
  return (
    <Pop onBack={onBack} name="model" title={t("model.title")} closeLabel={t("model.close")} onClose={onClose}>
      <div className="field"><label htmlFor="model-select">{t("model.model")}</label>
        <select id="model-select" value={thread?.model ?? ""} disabled={busy || models.length === 0} onChange={(event) => void actions.settings({ model: event.target.value })}>
          {/* A harness may sit on its own default, which is not one of the choices. */}
          {!models.some((option) => option.model === thread?.model) && <option value="">{t(thread?.modelSelectionStatus === "unknown" ? "model.unknown" : "model.default")}</option>}
          {models.map((option) => <option key={option.id} value={option.model}>{option.displayName || option.model}</option>)}
        </select>
      </div>
      {efforts.length > 0 && (
        <div className="field"><label htmlFor="effort-select">{t("model.effort")}</label>
          <select id="effort-select" value={thread?.effortSelectionStatus === "unknown" ? "__unknown" : thread?.reasoningEffort ?? ""} disabled={busy} onChange={(event) => void actions.settings({ reasoningEffort: event.target.value || null })}>
            {thread?.effortSelectionStatus === "unknown" && <option value="__unknown" disabled>{t("model.effortUnknown")}</option>}
            <option value="">{t("model.default")}</option>
            {efforts.map((option) => <option key={option.reasoningEffort} value={option.reasoningEffort}>{option.reasoningEffort}</option>)}
          </select>
        </div>
      )}
    </Pop>
  );
}

/** One composer entry; selection hands off to the existing panels and file picker. */
export function ComposerMenu({ modelName, canUpload, canGoal, canModel, hasGoal, onUpload, onGoal, onModel, onClose }: {
  modelName: string; canUpload: boolean; canGoal: boolean; canModel: boolean; hasGoal: boolean;
  onUpload: () => void; onGoal: () => void; onModel: () => void; onClose: () => void;
}) {
  const menu = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const frame = requestAnimationFrame(() => {
      const target = menu.current?.querySelector<HTMLButtonElement>("button:not(:disabled)") ?? menu.current?.parentElement?.querySelector<HTMLButtonElement>(".xbtn");
      target?.focus();
    });
    return () => cancelAnimationFrame(frame);
  }, []);
  return <section className="pop" id="pop-actions" aria-label={t("composer.actions")}>
    <header className="pop-h"><h3>{t("composer.actions")}</h3><button type="button" className="xbtn" aria-label={t("composer.closeActions")} onClick={onClose}><Icon name="close" /></button></header>
    <div className="composer-menu" role="menu" aria-label={t("composer.actions")} ref={menu} onKeyDown={event => {
      const items = Array.from(menu.current?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") ?? []);
      const index = items.indexOf(document.activeElement as HTMLButtonElement);
      const next = event.key === "Home" ? 0 : event.key === "End" ? items.length - 1 : event.key === "ArrowDown" ? (index + 1) % items.length : event.key === "ArrowUp" ? (index + items.length - 1) % items.length : null;
      if (next !== null) { event.preventDefault(); items[next]?.focus(); }
    }}>
      <button type="button" role="menuitem" disabled={!canUpload} title={t("images.limits")} onClick={onUpload}><Icon name="plus" /><span>{t("images.add")}{!canUpload && <small>{t("composer.unavailable")}</small>}</span></button>
      <button type="button" role="menuitem" disabled={!canGoal} onClick={onGoal}><Icon name="goal" /><span>{t(hasGoal ? "goal.title" : "goal.set")}{!canGoal && <small>{t("goal.noSupport")}</small>}</span></button>
      <button type="button" role="menuitem" disabled={!canModel} onClick={onModel}><Icon name="spark" /><span>{t("model.title")}<small>{modelName}{!canModel && ` · ${t("composer.unavailable")}`}</small></span></button>
    </div>
  </section>;
}
