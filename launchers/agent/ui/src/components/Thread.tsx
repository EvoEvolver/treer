import { Fragment, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { t, type MessageKey } from "../i18n";
import { useAppearance } from "../appearance";
import { useStore } from "../state/store";
import type { TurnDto } from "../types";
import { Icon } from "./Icon";
import { TurnView } from "./Timeline";

function GoalDone({ objective }: { objective: string }) {
  return <section className="goal-done" role="status"><Icon name="check" /><span>{t("goal.done", { objective })}</span></section>;
}

const STICK = 160; // px from the bottom that still counts as "following the conversation"

/** The conversation. It follows new content only while the reader is at the bottom; scrolling up shows "jump to latest". */
export function Thread() {
  const { state, actions } = useStore();
  const { look } = useAppearance();
  const { snapshot } = state;
  const ref = useRef<HTMLDivElement>(null);
  const stuck = useRef(true);
  const anchor = useRef<{ height: number; top: number } | null>(null);
  const seen = useRef<TurnDto[]>([]);
  const [away, setAway] = useState(false);
  const [fresh, setFresh] = useState(false);
  const turns = snapshot?.thread?.turns ?? [];
  const running = snapshot?.thread?.status === "running";
  const view = !snapshot ? (state.error ? "error" : "loading") : !state.connected ? "error" : turns.length === 0 ? "empty" : "chat";

  const onScroll = useCallback(() => {
    const element = ref.current;
    if (!element) return;
    const distance = element.scrollHeight - element.scrollTop - element.clientHeight;
    stuck.current = distance < STICK;
    setAway(!stuck.current);
    if (stuck.current) setFresh(false);
  }, []);

  useEffect(() => {
    const resized = () => {
      const element = ref.current;
      // Preserve a reader's position in older history, but keep the newest
      // message above the floating dock when its attachment tray grows.
      if (element && stuck.current) element.scrollTop = element.scrollHeight;
    };
    document.addEventListener("treer:dock-resize", resized);
    return () => document.removeEventListener("treer:dock-resize", resized);
  }, []);

  // After every update: keep the anchor when earlier history was prepended, follow the bottom when stuck, otherwise flag news.
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    const previous = seen.current;
    seen.current = turns;
    if (turns.length === previous.length && turns.every((turn, index) => turn === previous[index])) return; // nothing visible changed
    if (anchor.current) {
      element.scrollTop = anchor.current.top + element.scrollHeight - anchor.current.height;
      anchor.current = null;
    } else if (stuck.current) element.scrollTop = element.scrollHeight;
    else setFresh(true);
  }, [turns]);

  async function older() {
    const element = ref.current;
    if (element) anchor.current = { height: element.scrollHeight, top: element.scrollTop };
    if (!(await actions.loadOlder())) anchor.current = null; // otherwise the layout effect consumes it
  }
  function jump() {
    const element = ref.current;
    if (!element) return;
    stuck.current = true;
    element.scrollTo({ top: element.scrollHeight, behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
    setFresh(false);
  }

  return (
    <>
      <div className="thread" id="thread" ref={ref} role="log" aria-live="polite" aria-label={t("thread.label")} tabIndex={-1} data-view={view} onScroll={onScroll}>
        <div className="col">
          {view === "error" && (
            <div className="banner" role="alert"><Icon name="warn" /><div><b>{t("banner.disconnected")}</b> {state.error ?? t("banner.reconnecting")}</div><button type="button" className="link" onClick={actions.retry}>{t("banner.retry")}</button></div>
          )}
          {view === "empty" && (
            <div className="v-empty"><div className="empty"><span className="empty-mark" aria-hidden="true"><Icon name="agent" /></span><h2>{t(`empty.${look}.title` as MessageKey)}</h2><p>{t(`empty.${look}.text` as MessageKey)}</p></div></div>
          )}
          {view === "loading" && (
            <div className="v-loading" role="status" aria-label={t("loading.label")}><div className="sk sk-user" /><div className="sk sk-l1" /><div className="sk sk-l2" /><div className="sk sk-l3" /><div className="sk sk-row" /></div>
          )}
          <div className="chat">
            {snapshot?.thread?.hasOlderItems && (
              <div className="older"><button type="button" className="btn" disabled={state.loadingOlder} onClick={() => void older()}>{state.loadingOlder ? t("thread.loadingOlder") : t("thread.older")}</button></div>
            )}
            {turns.map((turn, index) => (
              <Fragment key={turn.id}>
                <TurnView turn={turn} latest={index === turns.length - 1} />
                {state.notices.filter((notice) => notice.afterTurn === turn.id).map((notice, n) => <GoalDone key={n} objective={notice.objective} />)}
              </Fragment>
            ))}
            {state.notices.filter((notice) => !turns.some((turn) => turn.id === notice.afterTurn)).map((notice, n) => <GoalDone key={`x${n}`} objective={notice.objective} />)}
            {running && <div className="working" role="status"><i className="spin" aria-hidden="true" /><span>{t("thread.working")}</span></div>}
          </div>
        </div>
      </div>
      {away && view === "chat" && (
        <button type="button" className="jump" id="jump" data-new={fresh || undefined} onClick={jump}><Icon name="down" /><span>{t("jump.latest")}</span></button>
      )}
    </>
  );
}
