import { useEffect, useRef, useState, type FormEvent } from "react";
import { t } from "../i18n";
import { currentGoal } from "../lib/goal";
import { useStore } from "../state/store";
import { Icon } from "./Icon";

/** The objective/budget form in a native modal dialog, which supplies the focus trap, Esc and backdrop. */
export function GoalEditor({ open, onClose, onSaved }: { open: boolean; onClose: () => void; onSaved: () => void }) {
  const { state, actions } = useStore();
  const dialog = useRef<HTMLDialogElement>(null);
  const objective = useRef<HTMLTextAreaElement>(null);
  const [text, setText] = useState("");
  const [budget, setBudget] = useState("");
  const [roundBudget, setRoundBudget] = useState("");
  const [error, setError] = useState<string | null>(null);
  const snapshot = state.snapshot;
  const goal = snapshot ? currentGoal(snapshot) : null;
  const budgetSupported = Boolean(snapshot?.harness.capabilities.goal?.budget);

  useEffect(() => {
    const element = dialog.current;
    if (!element) return;
    if (open && !element.open) {
      setText(goal?.objective ?? ""); setBudget(goal?.tokenBudget != null ? String(goal.tokenBudget) : ""); setError(null);
      setRoundBudget(goal?.roundBudget != null ? String(goal.roundBudget) : "");
      element.showModal();
      objective.current?.focus();
    } else if (!open && element.open) element.close();
    // The form is seeded when it opens, not on every snapshot.
  }, [open]); // eslint-disable-line react-hooks/exhaustive-deps

  async function submit(event: FormEvent) {
    event.preventDefault();
    const trimmed = text.trim();
    if (!trimmed || state.goalBusy) return;
    const input: { objective: string; tokenBudget?: number | null; roundBudget?: number | null } = { objective: trimmed };
    if (budgetSupported) {
      input.tokenBudget = budget === "" ? null : Number(budget);
      if (input.tokenBudget != null && (!Number.isSafeInteger(input.tokenBudget) || input.tokenBudget <= 0)) { setError(t("goal.editor.budgetError")); return; }
      input.roundBudget = roundBudget === "" ? null : Number(roundBudget);
      if (input.roundBudget != null && (!Number.isSafeInteger(input.roundBudget) || input.roundBudget < 1 || input.roundBudget > 10000)) { setError(t("goal.editor.roundBudgetError")); return; }
    }
    setError(null);
    const failure = await actions.putGoal(input);
    if (failure) setError(failure); else onSaved();
  }

  return (
    <dialog ref={dialog} className="goal-dialog" aria-labelledby="goal-editor-title" onClose={() => { if (open) onClose(); }} onCancel={event => { event.preventDefault(); onClose(); }} onClick={event => { if (event.target === event.currentTarget) onClose(); }}>
      <form onSubmit={(event) => void submit(event)}>
        <header className="dialog-h"><h2 id="goal-editor-title">{goal ? t("goal.editor.editTitle") : t("goal.editor.setTitle")}</h2><button type="button" className="ib" aria-label={t("goal.editor.close")} onClick={onClose}><Icon name="close" /></button></header>
        <div className="field"><label htmlFor="goal-objective">{t("goal.editor.objective")}</label><textarea id="goal-objective" ref={objective} rows={3} required value={text} onChange={(event) => setText(event.target.value)} /></div>
        {budgetSupported && <>
          <div className="field"><label htmlFor="goal-budget">{t("goal.editor.budget")}</label><input id="goal-budget" type="number" min={1} step={1} inputMode="numeric" placeholder={t("goal.unlimited")} aria-describedby="goal-budget-help" value={budget} onChange={(event) => setBudget(event.target.value)} /></div>
          <div className="field"><label htmlFor="goal-round-budget">{t("goal.editor.roundBudget")}</label><input id="goal-round-budget" type="number" min={1} max={10000} step={1} inputMode="numeric" placeholder={t("goal.unlimited")} aria-describedby="goal-budget-help" value={roundBudget} onChange={(event) => setRoundBudget(event.target.value)} /></div>
          <p id="goal-budget-help" className="meta">{t("goal.editor.unlimitedHelp")}</p>
        </>}
        {error && <p className="field-error" role="alert">{error}</p>}
        <div className="dialog-actions"><button type="button" className="btn" onClick={onClose}>{t("goal.editor.cancel")}</button><button type="submit" className="btn primary" disabled={state.goalBusy}>{t("goal.editor.save")}</button></div>
      </form>
    </dialog>
  );
}
