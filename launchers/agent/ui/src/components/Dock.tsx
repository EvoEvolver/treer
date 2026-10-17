import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import { t } from "../i18n";
import { useStore } from "../state/store";
import { GoalEditor } from "./GoalEditor";
import { Icon } from "./Icon";
import { GoalPop, ModelPop, PlanPop, QueuePop } from "./Pops";
import { currentGoal } from "../lib/goal";
import { latestPlan, Strip, type PopName } from "./Strip";
import { ImageDrafts, useImageDrafts } from "./ImageDrafts";

/** The composer: one real textarea (compact when idle, expanded when focused or multi-line), the status strip, and one popover at a time. */
export function Dock() {
  const { state, actions, dispatch } = useStore();
  const { snapshot } = state;
  const [text, setText] = useState("");
  const images = useImageDrafts(message => dispatch({ type: "error", message }));
  const picker = useRef<HTMLInputElement>(null);
  // Only the field's own focus expands the composer. The layout must not change between pointerdown and click, or a button
  // pressed while the field is focused would move away from under the pointer; `hold` keeps it expanded until the click ends.
  const [focused, setFocused] = useState(false);
  const [dragging, setDragging] = useState(false);
  const hold = useRef(false);
  const composer = useRef<HTMLDivElement>(null);
  const [pop, setPop] = useState<PopName | null>(null);
  const [editor, setEditor] = useState(false);
  const [pendingClear, setPendingClear] = useState<{ prompt: string | null } | null>(null);
  const area = useRef<HTMLTextAreaElement>(null);
  const dock = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLElement | null>(null);
  const running = snapshot?.thread?.status === "running";
  const canSteer = Boolean(snapshot?.harness.capabilities.steer);
  const name = snapshot?.harness.name || t("app.name");
  const expanded = focused || text.includes("\n") || text.length > 70;
  const canSend = Boolean(snapshot?.ready) && !state.submitting && !images.busy && (text.trim().length > 0 || images.images.length > 0);
  const thread = snapshot?.thread;
  const models = snapshot?.modelOptions ?? [];
  const effort = thread?.reasoningEffort ?? "";
  const modelName = models.find((option) => option.model === thread?.model)?.displayName || thread?.model || t("model.default");

  useLayoutEffect(() => {
    const element = area.current;
    if (!element) return;
    element.style.height = "auto";
    element.style.height = `${Math.min(element.scrollHeight, 180)}px`;
  }, [text, expanded]);
  // Keeps --dock-h in step with the composer so the thread and "jump to latest" clear it.
  useEffect(() => {
    const element = dock.current;
    if (!element) return;
    const observer = new ResizeObserver(() => {
      document.documentElement.style.setProperty("--dock-h", `${element.offsetHeight}px`);
      document.dispatchEvent(new Event("treer:dock-resize"));
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  function release() {
    hold.current = false;
    if (!composer.current?.contains(document.activeElement)) setFocused(false);
  }
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const onUp = () => { clearTimeout(timer); timer = setTimeout(release, 250); };
    document.addEventListener("pointerup", onUp);
    document.addEventListener("pointercancel", onUp);
    return () => { clearTimeout(timer); document.removeEventListener("pointerup", onUp); document.removeEventListener("pointercancel", onUp); };
  }, []);

  // A popover whose subject disappeared (queue drained, goal cleared, no plan) closes, so it never reopens by itself later.
  const hasGoal = Boolean(snapshot && currentGoal(snapshot)), hasPlan = Boolean(snapshot && latestPlan(snapshot.thread?.turns ?? [])), hasQueue = (snapshot?.queue.length ?? 0) > 0;
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
      if (!target?.closest(".pop, .strip, .model")) closePop(false);
    };
    document.addEventListener("keydown", onKey);
    document.addEventListener("pointerdown", onPointer);
    return () => { document.removeEventListener("keydown", onKey); document.removeEventListener("pointerdown", onPointer); };
  }, [pop]); // eslint-disable-line react-hooks/exhaustive-deps

  /** `/goal`, `/goal clear` and `/goal <objective>` manage the goal instead of being sent as chat. Returns true when handled. */
  async function goalCommand(prompt: string): Promise<boolean> {
    if (!/^\/goal(?:\s|$)/.test(prompt)) return false;
    if (!snapshot?.harness.capabilities.goal) { dispatch({ type: "error", message: t("goal.noSupport") }); return true; }
    const objective = prompt.slice(5).trim();
    if (!objective) setEditor(true);
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
    const ids = images.images.map(image => image.id);
    if (await actions.prompt(prompt, mode, ids)) { images.sent(ids); setText((current) => (current.trim() === prompt ? "" : current)); }
  }
  function onKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.nativeEvent.isComposing) return;
    if (event.key === "Tab" && !event.shiftKey && (text.trim() || images.images.length)) { event.preventDefault(); void submit("queue"); }
    else if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void submit("steer"); }
  }
  const sendLabel = running ? (canSteer ? t("composer.steer") : t("composer.queue")) : t("composer.send");
  const sendShort = running ? (canSteer ? t("composer.steerShort") : t("composer.queueShort")) : t("composer.sendShort");
  const hint = running ? (canSteer ? t("composer.hint.steer") : t("composer.hint.queueOnly")) : t("composer.hint.idle");
  return (
    <div className="dock" id="dock" ref={dock}>
      <div className="dock-in">
        {(state.error || state.notice) && <p className={state.error ? "feedback error" : "feedback"} role={state.error ? "alert" : "status"}>{state.error ?? state.notice}</p>}
        {pop === "plan" && <PlanPop onClose={() => closePop()} />}
        {pop === "queue" && <QueuePop onClose={() => closePop()} />}
        {pop === "model" && <ModelPop onClose={() => closePop()} />}
        {pop === "goal" && (
          <GoalPop confirming={pendingClear !== null} onClose={() => closePop()} onEdit={() => { closePop(false); setEditor(true); }} onAskClear={() => setPendingClear({ prompt: null })}
            onKeep={() => setPendingClear(null)} onCleared={() => { if (pendingClear?.prompt) setText((current) => (current.trim() === pendingClear.prompt ? "" : current)); closePop(); }} />
        )}
        <div className={`composer${expanded ? " exp" : ""}${dragging ? " image-drop" : ""}`} id="composer" ref={composer} onPointerDown={() => { hold.current = true; }} onClick={release}
          onDragOver={event => { if (event.dataTransfer.types.includes("Files")) { event.preventDefault(); setDragging(true); } }}
          onDragLeave={event => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setDragging(false); }}
          onDrop={event => { setDragging(false); if (event.dataTransfer.files.length) { event.preventDefault(); if (!state.submitting) void images.add(Array.from(event.dataTransfer.files)); } }}
          onBlur={(event) => { if (!hold.current && !event.currentTarget.contains(event.relatedTarget)) setFocused(false); }}>
          <Strip pop={pop} onToggle={toggle} onEditGoal={() => setEditor(true)} />
          {(images.images.length > 0 || images.busy) && <ImageDrafts images={images.images} busy={images.busy} disabled={state.submitting} fallback={!snapshot?.harness.capabilities.images} onRemove={id => void images.remove(id)} />}
          <div className={expanded ? "box exp" : "box"} id="box">
            <span className="prompt-glyph" aria-hidden="true">❯</span>
            <textarea id="ta" ref={area} rows={1} value={text} placeholder={t("composer.placeholder", { name })} aria-label={t("composer.placeholder", { name })} aria-describedby="hint"
              onPaste={event => { const files = Array.from(event.clipboardData.files); if (files.length) { event.preventDefault(); if (!state.submitting) void images.add(files); } }}
              onChange={(event) => setText(event.target.value)} onKeyDown={onKeyDown} onFocus={() => setFocused(true)} />
            <div className="ctl">
              <input ref={picker} type="file" className="sr-only" tabIndex={-1} aria-hidden="true" accept="image/png,image/jpeg,image/webp,image/gif" multiple onChange={event => { if (!state.submitting) void images.add(Array.from(event.currentTarget.files ?? [])); event.currentTarget.value = ""; }} />
              <button type="button" className="btn image-add" disabled={!snapshot?.ready || snapshot.attachmentsAvailable === false || state.submitting || images.busy} aria-label={t("images.add")} title={t("images.limits")} onClick={() => picker.current?.click()}>＋</button>
              {thread && (models.length > 0
                ? <button type="button" className="model" aria-haspopup="dialog" aria-expanded={pop === "model"} aria-controls="pop-model" aria-label={t("model.chip", { model: modelName, effort: effort || t("model.default") })} onClick={(event) => toggle("model", event.currentTarget)}><span>{modelName}</span>{effort && <small>{effort}</small>}</button>
                : thread.model && <span className="model static"><span>{thread.model}</span>{effort && <small>{effort}</small>}</span>)}
              {running && canSteer && <button type="button" className="btn queueb" disabled={!canSend} onClick={() => void submit("queue")}>{t("composer.queueShort")}</button>}
              <button type="button" className="send" id="send" aria-label={sendLabel} title={sendLabel} disabled={!canSend} onClick={() => void submit("steer")}>
                <span className="send-t">{sendShort}</span><Icon name="up" />
              </button>
              <button type="button" className="stopb" aria-label={t("composer.stop")} title={t("composer.stop")} disabled={!running} onClick={() => void actions.interrupt()}><Icon name="stop" /></button>
            </div>
          </div>
          <p className="hint shortcut-hint" id="hint">{hint}</p>
          {snapshot?.attachmentsAvailable === false && <p className="hint image-storage-status" role="status">{t(snapshot.attachmentStorageStatus === "windows_requires_administrator" ? "images.storageAdministrator" : snapshot.attachmentStorageStatus === "agent_private_state_inside_cwd" ? "images.storageInside" : snapshot.attachmentStorageStatus === "agent_private_state_inside_vcs_worktree" ? "images.storageVcs" : snapshot.attachmentStorageStatus === "agent_private_state_unsafe" ? "images.storageUnsafe" : "images.storageUnavailable")}</p>}
          {snapshot?.attachmentsAvailable && images.images.length === 0 && <p className="hint">{t("images.limits")}</p>}
        </div>
      </div>
      <GoalEditor open={editor} onClose={() => setEditor(false)} onSaved={() => setEditor(false)} />
    </div>
  );
}
