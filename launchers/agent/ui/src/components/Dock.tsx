import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import { t } from "../i18n";
import { useStore } from "../state/store";
import { Icon } from "./Icon";

/** Stage-1 composer: one real textarea (compact when idle, expanded when focused or multi-line) with send/steer/queue/stop. */
export function Dock() {
  const { state, actions } = useStore();
  const { snapshot } = state;
  const [text, setText] = useState("");
  const [focused, setFocused] = useState(false);
  const area = useRef<HTMLTextAreaElement>(null);
  const dock = useRef<HTMLDivElement>(null);
  const running = snapshot?.thread?.status === "running";
  const canSteer = Boolean(snapshot?.harness.capabilities.steer);
  const name = snapshot?.harness.name || t("app.name");
  const expanded = focused || text.includes("\n") || text.length > 70;
  const canSend = Boolean(snapshot?.ready) && !state.submitting && text.trim().length > 0;

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
    const observer = new ResizeObserver(() => document.documentElement.style.setProperty("--dock-h", `${element.offsetHeight}px`));
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  async function submit(mode: "steer" | "queue") {
    const prompt = text.trim();
    if (!prompt || !canSend) return;
    if (await actions.prompt(prompt, mode)) setText("");
  }
  function onKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.nativeEvent.isComposing) return;
    if (event.key === "Tab" && !event.shiftKey && text.trim()) { event.preventDefault(); void submit("queue"); }
    else if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void submit("steer"); }
  }
  const sendLabel = running ? (canSteer ? t("composer.steer") : t("composer.queue")) : t("composer.send");
  const sendShort = running ? (canSteer ? t("composer.steerShort") : t("composer.queueShort")) : t("composer.sendShort");
  const hint = running ? (canSteer ? t("composer.hint.steer") : t("composer.hint.queueOnly")) : t("composer.hint.idle");
  return (
    <div className="dock" id="dock" ref={dock}>
      <div className="dock-in">
        {(state.error || state.notice) && <p className={state.error ? "feedback error" : "feedback"} role={state.error ? "alert" : "status"}>{state.error ?? state.notice}</p>}
        <div className={expanded ? "composer exp" : "composer"} id="composer">
          <div className={expanded ? "box exp" : "box"} id="box">
            <span className="prompt-glyph" aria-hidden="true">❯</span>
            <textarea id="ta" ref={area} rows={1} value={text} placeholder={t("composer.placeholder", { name })} aria-label={t("composer.placeholder", { name })} aria-describedby="hint"
              onChange={(event) => setText(event.target.value)} onKeyDown={onKeyDown} onFocus={() => setFocused(true)} onBlur={() => setFocused(false)} />
            <div className="ctl">
              <button type="button" className="send" id="send" aria-label={sendLabel} title={sendLabel} disabled={!canSend} onClick={() => void submit("steer")}>
                <span className="send-t">{sendShort}</span><Icon name="up" />
              </button>
              <button type="button" className="stopb" aria-label={t("composer.stop")} title={t("composer.stop")} disabled={!running} onClick={() => void actions.interrupt()}><Icon name="stop" /></button>
            </div>
          </div>
          <p className="hint" id="hint">{hint}</p>
        </div>
      </div>
    </div>
  );
}
