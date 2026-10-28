import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import { t } from "../i18n";
import { useNow } from "../lib/clock";
import { duration } from "../lib/format";
import { currentGoal, goalStatus, needsAttention } from "../lib/goal";
import { latestPlan, liveStep } from "../lib/run";
import { onDraftInsert } from "../state/draft";
import { useStore } from "../state/store";
import type { TurnDto } from "../types";
import { GoalEditor } from "./GoalEditor";
import { Icon } from "./Icon";
import { ImageDrafts, useImageDrafts } from "./ImageDrafts";
import { GoalPop, ModelPop, PlanPop, QueuePop } from "./Pops";

type PopName = "goal" | "plan" | "queue" | "model";

// When a turn has no provider start time, its clock starts when this page first saw it running.
const firstSeen = new Map<string, number>();
function turnStart(turn: TurnDto): number {
  const parsed = turn.startedAt ? Date.parse(turn.startedAt) : NaN;
  if (Number.isFinite(parsed)) return parsed;
  let seen = firstSeen.get(turn.id);
  if (seen === undefined) { seen = Date.now(); firstSeen.set(turn.id, seen); }
  return seen;
}

/** Live run state above the composer: what is happening, for how long, goal, plan, queue, and Stop. */
function RunBar({ pop, onToggle }: { pop: PopName | null; onToggle: (name: PopName, trigger: HTMLElement) => void }) {
  const { state, actions } = useStore();
  const snapshot = state.snapshot;
  const thread = snapshot?.thread;
  const running = thread?.status === "running";
  const live = running ? thread?.turns.find((turn) => turn.id === thread.activeTurnId) ?? thread?.turns.at(-1) : undefined;
  const now = useNow(running);
  if (!snapshot) return null;
  const goal = snapshot.harness.capabilities.goal ? currentGoal(snapshot) : null;
  const plan = latestPlan(thread?.turns ?? []);
  const steps = plan?.steps ?? [];
  const done = steps.filter((step) => step.status === "completed").length;
  // Only the latest turn's plan, while it runs or has open steps; an older or abandoned plan does not linger.
  const showPlan = Boolean(plan && thread?.turns.at(-1)?.items.includes(plan)) && (running || done < steps.length);
  const queued = snapshot.queue.length;
  if (!running && !goal && !showPlan && !queued) return null;
  const attention = needsAttention(snapshot);
  return (
    <div className="runbar" role="group" aria-label={t("run.label")}>
      {running && (
        <span className="live">
          <i className="spin" aria-hidden="true" />
          <b>{t("run.working", { time: live ? duration(now - turnStart(live)) : "" })}</b>
          <span className="step">{liveStep(live)}</span>
        </span>
      )}
      <span className="segs">
        {goal && (
          <button type="button" className={attention ? "seg attn" : "seg"} aria-label={`${t("goal.title")}: ${t(`goal.status.${goalStatus(goal)}`)}`} aria-expanded={pop === "goal"} aria-controls="pop-goal" onClick={(event) => onToggle("goal", event.currentTarget)}>
            <Icon name={attention ? "warn" : "goal"} /><span className="seg-t">{t("goal.title")}</span><span className="seg-v">{t(`goal.status.${goalStatus(goal)}`)}</span>
          </button>
        )}
        {showPlan && <button type="button" className="seg" aria-label={`${t("plan.title")} ${done}/${steps.length}`} aria-expanded={pop === "plan"} aria-controls="pop-plan" onClick={(event) => onToggle("plan", event.currentTarget)}><Icon name="plan" /><span className="seg-t">{t("plan.title")}</span><span className="seg-v">{done}/{steps.length}</span></button>}
        {queued > 0 && <button type="button" className="seg" aria-label={`${t("queue.segment")}: ${queued}`} aria-expanded={pop === "queue"} aria-controls="pop-queue" onClick={(event) => onToggle("queue", event.currentTarget)}><Icon name="queue" /><span className="seg-t">{t("queue.segment")}</span><span className="seg-v">{queued}</span></button>}
      </span>
      {running && <button type="button" className="btn stop" onClick={() => void actions.interrupt()}><Icon name="stop" />{t("composer.stop")}</button>}
    </div>
  );
}

/** The footer: run bar, one popover at a time, and a stable two-row composer that never moves its buttons. */
export function Composer() {
  const { state, actions, dispatch } = useStore();
  const { snapshot } = state;
  const [text, setText] = useState("");
  const images = useImageDrafts((message) => dispatch({ type: "error", message }));
  const picker = useRef<HTMLInputElement>(null);
  const [dragging, setDragging] = useState(false);
  const [pop, setPop] = useState<PopName | null>(null);
  const [editor, setEditor] = useState(false);
  const [pendingClear, setPendingClear] = useState<{ prompt: string | null } | null>(null);
  const area = useRef<HTMLTextAreaElement>(null);
  const trigger = useRef<HTMLElement | null>(null);
  const thread = snapshot?.thread;
  const running = thread?.status === "running";
  const canSteer = Boolean(snapshot?.harness.capabilities.steer);
  const name = snapshot?.harness.name || t("app.name");
  const canSend = Boolean(snapshot?.ready) && !state.submitting && !images.busy && (text.trim().length > 0 || images.images.length > 0);
  const canUpload = Boolean(snapshot?.ready) && snapshot?.attachmentsAvailable !== false && !state.submitting && !images.busy;
  const models = snapshot?.modelOptions ?? [];
  const modelName = thread?.modelSelectionStatus === "unknown" ? t("model.unknown") : models.find((option) => option.model === thread?.model)?.displayName || thread?.model || t("model.default");
  const effort = thread?.effortSelectionStatus === "unknown" ? t("model.effortUnknown") : thread?.reasoningEffort;

  useLayoutEffect(() => {
    const element = area.current;
    if (!element) return;
    element.style.height = "auto";
    element.style.height = `${Math.min(element.scrollHeight, 200)}px`;
  }, [text]);
  useEffect(() => onDraftInsert((insert) => {
    setText((current) => `${current}${current && !/\s$/.test(current) ? " " : ""}${insert} `);
    requestAnimationFrame(() => { const element = area.current; if (element) { element.focus(); element.setSelectionRange(element.value.length, element.value.length); } });
  }), []);

  // A popover whose subject disappeared (queue drained, goal cleared, no plan) closes, so it never reopens by itself later.
  const hasGoal = Boolean(snapshot && currentGoal(snapshot)), hasPlan = Boolean(snapshot && latestPlan(thread?.turns ?? [])), hasQueue = (snapshot?.queue.length ?? 0) > 0;
  useEffect(() => {
    if ((pop === "goal" && !hasGoal) || (pop === "plan" && !hasPlan) || (pop === "queue" && !hasQueue)) { setPop(null); setPendingClear(null); }
  }, [pop, hasGoal, hasPlan, hasQueue]);

  function closePop(returnFocus = true) {
    setPop(null);
    setPendingClear(null);
    if (returnFocus) trigger.current?.focus();
  }
  function toggle(next: PopName, element: HTMLElement) {
    trigger.current = element;
    setPendingClear(null);
    setPop((current) => (current === next ? null : next));
  }
  useEffect(() => {
    if (!pop) return;
    const onKey = (event: globalThis.KeyboardEvent) => { if (event.key === "Escape" && !document.querySelector("dialog[open]")) closePop(); };
    const onPointer = (event: PointerEvent) => {
      const target = event.target as Element | null;
      if (!target?.closest(".pop, .runbar, .ctl")) closePop(false);
    };
    document.addEventListener("keydown", onKey);
    document.addEventListener("pointerdown", onPointer);
    return () => { document.removeEventListener("keydown", onKey); document.removeEventListener("pointerdown", onPointer); };
  }, [pop]); // eslint-disable-line react-hooks/exhaustive-deps

  function openGoal(element: HTMLElement) {
    trigger.current = element;
    if (hasGoal) toggle("goal", element); else { setPop(null); setEditor(true); }
  }
  function closeEditor() {
    setEditor(false);
    requestAnimationFrame(() => trigger.current?.focus());
  }

  /** `/goal`, `/goal clear` and `/goal <objective>` manage the goal instead of being sent as chat. Returns true when handled. */
  async function goalCommand(prompt: string): Promise<boolean> {
    if (!/^\/goal(?:\s|$)/.test(prompt)) return false;
    if (!snapshot?.harness.capabilities.goal) { dispatch({ type: "error", message: t("goal.noSupport") }); return true; }
    const objective = prompt.slice(5).trim();
    if (!objective) { trigger.current = area.current; setEditor(true); }
    else if (objective === "clear") { setPop("goal"); setPendingClear({ prompt }); }
    else {
      const failure = await actions.putGoal({ objective });
      if (failure) dispatch({ type: "error", message: failure }); else setText((current) => (current.trim() === prompt ? "" : current));
    }
    return true;
  }
  async function submit(mode: "steer" | "queue") {
    const prompt = text.trim();
    if (!canSend) return;
    if (!images.images.length && await goalCommand(prompt)) return;
    // Clear the draft only if the reader has not typed something new while the request was in flight.
    const ids = images.images.map((image) => image.id);
    if (await actions.prompt(prompt, mode, ids)) { images.sent(ids); setText((current) => (current.trim() === prompt ? "" : current)); }
  }
  function onKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.nativeEvent.isComposing) return;
    if (event.key === "Tab" && !event.shiftKey && (text.trim() || images.images.length)) { event.preventDefault(); void submit("queue"); }
    else if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void submit("steer"); }
  }
  const primary = running ? (canSteer ? t("composer.steerShort") : t("composer.queueShort")) : t("composer.sendShort");
  const primaryTitle = running ? (canSteer ? t("composer.steer") : t("composer.queue")) : t("composer.send");
  return (
    <footer className="foot" id="dock">
      <div className="foot-in">
        {(state.error || state.notice) && (
          <p className={state.error ? "feedback error" : "feedback"} role={state.error ? "alert" : "status"}>
            <Icon name={state.error ? "warn" : "check"} /><span>{state.error ?? state.notice}</span>
            <button type="button" className="ib" aria-label={t("feedback.dismiss")} onClick={() => dispatch({ type: state.error ? "error" : "notice", message: null })}><Icon name="close" /></button>
          </p>
        )}
        {pop === "plan" && <PlanPop onClose={() => closePop()} />}
        {pop === "queue" && <QueuePop onClose={() => closePop()} />}
        {pop === "model" && <ModelPop onClose={() => closePop()} />}
        {pop === "goal" && (
          <GoalPop confirming={pendingClear !== null} onClose={() => closePop()} onEdit={() => { setPop(null); setEditor(true); }} onAskClear={() => setPendingClear({ prompt: null })}
            onKeep={() => setPendingClear(null)} onCleared={() => { if (pendingClear?.prompt) setText((current) => (current.trim() === pendingClear.prompt ? "" : current)); closePop(); }} />
        )}
        <RunBar pop={pop} onToggle={toggle} />
        <div className={`composer${dragging ? " drop" : ""}`} id="composer"
          onDragOver={(event) => { if (event.dataTransfer.types.includes("Files")) { event.preventDefault(); setDragging(true); } }}
          onDragLeave={(event) => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setDragging(false); }}
          onDrop={(event) => { setDragging(false); if (event.dataTransfer.files.length) { event.preventDefault(); if (!state.submitting) void images.add(Array.from(event.dataTransfer.files)); } }}>
          {(images.images.length > 0 || images.busy) && <ImageDrafts images={images.images} busy={images.busy} disabled={state.submitting} fallback={!snapshot?.harness.capabilities.images} onRemove={(id) => void images.remove(id)} />}
          <textarea id="ta" ref={area} rows={1} value={text} placeholder={running && canSteer ? t("composer.placeholderRunning", { name }) : t("composer.placeholder", { name })} aria-label={t("composer.placeholder", { name })} aria-describedby="composer-hint"
            onPaste={(event) => { const files = Array.from(event.clipboardData.files); if (files.length) { event.preventDefault(); if (!state.submitting) void images.add(files); } }}
            onChange={(event) => setText(event.target.value)} onKeyDown={onKeyDown} />
          <div className="ctl">
            <input ref={picker} type="file" className="sr-only" tabIndex={-1} aria-hidden="true" accept="image/png,image/jpeg,image/webp,image/gif" multiple onChange={(event) => { if (!state.submitting) void images.add(Array.from(event.currentTarget.files ?? [])); event.currentTarget.value = ""; }} />
            <button type="button" className="tool" disabled={!canUpload} title={canUpload ? t("images.attachTitle") : snapshot?.attachmentsAvailable === false ? t("images.storageUnavailable") : t("composer.unavailable")} onClick={() => picker.current?.click()}><Icon name="attach" /><span className="tool-t">{t("images.add")}</span></button>
            {snapshot?.harness.capabilities.goal && <button type="button" className="tool" aria-haspopup={hasGoal ? undefined : "dialog"} aria-expanded={hasGoal ? pop === "goal" : undefined} aria-controls={hasGoal ? "pop-goal" : undefined} onClick={(event) => openGoal(event.currentTarget)}><Icon name="goal" /><span className="tool-t">{hasGoal ? t("goal.title") : t("goal.set")}</span></button>}
            {thread && models.length > 0 && <button type="button" className="chip" aria-expanded={pop === "model"} aria-controls="pop-model" aria-label={t("model.chip", { model: modelName, effort: effort || t("model.default") })} onClick={(event) => toggle("model", event.currentTarget)}><span className="chip-m">{modelName}</span>{effort && <span className="chip-e">{effort}</span>}</button>}
            <span className="grow" />
            {running && canSteer && <button type="button" className="btn queue" disabled={!canSend} title={t("composer.queue")} onClick={() => void submit("queue")}>{t("composer.queueShort")}<kbd>Tab</kbd></button>}
            <button type="button" className="btn primary send" id="send" disabled={!canSend} title={primaryTitle} aria-label={primaryTitle} onClick={() => void submit("steer")}>{primary}<kbd aria-hidden="true">↵</kbd></button>
          </div>
        </div>
        <p className="hint" id="composer-hint">{running ? (canSteer ? t("composer.hint.steer") : t("composer.hint.queueOnly")) : t("composer.hint.idle")}</p>
        {snapshot?.attachmentsAvailable === false && <p className="hint" role="status">{t("images.storageUnavailable")}</p>}
      </div>
      <GoalEditor open={editor} onClose={closeEditor} onSaved={closeEditor} />
    </footer>
  );
}
