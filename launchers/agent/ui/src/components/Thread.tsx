import { Fragment, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { t } from "../i18n";
import { decideScroll, INTENT_MS, type ScrollSample } from "../lib/follow";
import { duration } from "../lib/format";
import { useStore } from "../state/store";
import type { Snapshot, ThreadState } from "../types";
import { Icon } from "./Icon";
import { TurnView } from "./Timeline";

const SEEN_KEY = "treer-launcher-seen:";
const readSeen = (thread: string) => { try { return localStorage.getItem(SEEN_KEY + thread); } catch { return null; } };
const writeSeen = (thread: string, turn: string) => { try { localStorage.setItem(SEEN_KEY + thread, turn); } catch { /* storage unavailable */ } };

function GoalDone({ objective }: { objective: string }) {
  return <section className="notice ok" role="status"><Icon name="check" /><span>{t("goal.done", { objective })}</span></section>;
}

/** Provider restarts and storage failures, stated in words with what happens to unconfirmed messages. */
function RecoveryBanner({ snapshot }: { snapshot: Snapshot }) {
  const recovery = snapshot.runtime.recovery;
  const blocked = snapshot.runtime.state === "blocked" || recovery?.state === "storage_failure";
  const restarting = recovery?.state === "restarting" || (!snapshot.ready && Boolean(snapshot.runtime.error));
  if (!blocked && !restarting) return null;
  const unresolved = recovery?.unresolved?.length ?? 0;
  const next = recovery?.nextRestartInMs;
  const detail = snapshot.runtime.error || recovery?.lastFailure;
  return (
    <section className={blocked ? "banner bad" : "banner warn"} role="alert">
      <Icon name="warn" />
      <div>
        <b>{blocked ? t("recovery.blocked") : t("recovery.restarting")}</b>
        {detail && <p className="banner-detail">{detail}</p>}
        <p className="banner-meta">{[recovery?.restarts ? t("recovery.restarts", { count: recovery.restarts }) : "", next != null && next > 0 ? t("recovery.next", { time: duration(next) }) : ""].filter(Boolean).join(" · ")}</p>
        {unresolved > 0 && <p>{t("recovery.unresolved", { count: unresolved })}</p>}
        <p className="banner-meta banner-detail">{t("recovery.kept")}</p>
      </div>
    </section>
  );
}

/**
 * The conversation. It opens at the latest message and follows new content while the reader is there; only the reader
 * scrolling up stops following (see lib/follow). A hidden frame or tab keeps following and is put back on show.
 */
export function Thread() {
  const { state, actions } = useStore();
  const { snapshot } = state;
  const stale = Boolean(state.staleThread) && (!snapshot?.thread || (!snapshot.ready && snapshot.thread.turns.length === 0));
  const thread: ThreadState | null = stale ? state.staleThread ?? null : snapshot?.thread ?? null;
  const scroller = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  const last = useRef<ScrollSample>({ top: 0, height: 0, client: 0 });
  const readerAt = useRef(-Infinity); // last wheel/touch/key/pointer input on the conversation
  const dragging = useRef(false); // pointer held down, e.g. on the scrollbar
  const hiddenWatch = useRef(0);
  const anchor = useRef<{ height: number; top: number } | null>(null);
  const initialThread = useRef<string | null>(null);
  const previousPage = useRef(false);
  const [away, setAway] = useState(false);
  const [fresh, setFresh] = useState(false);
  const [marker, setMarker] = useState<{ thread: string; after: string } | null>(null);
  const turns = thread?.turns ?? [];
  const running = thread?.status === "running";
  const view = !snapshot && !stale ? (state.error ? "error" : "loading") : turns.length === 0 ? (stale ? "loading" : "empty") : "chat";

  const sample = (element: HTMLElement): ScrollSample => ({ top: element.scrollTop, height: element.scrollHeight, client: element.clientHeight });
  const remember = (element: HTMLElement) => { const now = sample(element); if (now.client > 0) last.current = now; };

  // While hidden (zero size) nothing is measured or moved. Frames hidden by the Console are not rendered at all, so no
  // resize or scroll event is guaranteed on show: poll one animation frame at a time (frames do not run while hidden).
  const restoreRef = useRef<() => void>(() => undefined);
  const whenShown = useCallback(() => {
    if (hiddenWatch.current) return;
    const check = () => {
      const element = scroller.current;
      if (!element) { hiddenWatch.current = 0; return; }
      if (element.clientHeight === 0) { hiddenWatch.current = requestAnimationFrame(check); return; }
      hiddenWatch.current = 0;
      restoreRef.current();
    };
    hiddenWatch.current = requestAnimationFrame(check);
  }, []);
  useEffect(() => () => cancelAnimationFrame(hiddenWatch.current), []);

  const toBottom = useCallback(() => {
    const element = scroller.current;
    if (!element) return;
    if (element.clientHeight === 0) { whenShown(); return; }
    element.scrollTop = element.scrollHeight;
    remember(element);
  }, [whenShown]);
  const restore = useCallback(() => {
    const element = scroller.current;
    if (!element) return;
    if (pinned.current) { toBottom(); return; }
    element.scrollTop = last.current.top;
    remember(element);
  }, [toBottom]);
  restoreRef.current = restore;

  const onScroll = useCallback(() => {
    const element = scroller.current;
    if (!element) return;
    const reader = dragging.current || performance.now() - readerAt.current < INTENT_MS;
    const decision = decideScroll({ pinned: pinned.current, last: last.current }, sample(element), reader);
    if (decision === "hidden") { whenShown(); return; }
    if (decision === "follow" || decision === "restore") { restore(); return; }
    if (decision === "pin") pinned.current = true;
    if (decision === "unpin") pinned.current = false;
    remember(element);
    setAway(!pinned.current);
    if (pinned.current) setFresh(false);
  }, [whenShown, restore]);

  // The reader's own input is what may unpin; programmatic and layout scrolling never does.
  useEffect(() => {
    const element = scroller.current;
    if (!element) return;
    const mark = () => { readerAt.current = performance.now(); };
    const down = () => { dragging.current = true; mark(); };
    const up = () => { if (dragging.current) { dragging.current = false; mark(); } };
    const options = { passive: true } as const;
    for (const type of ["wheel", "touchstart", "touchmove", "keydown"]) element.addEventListener(type, mark, options);
    element.addEventListener("pointerdown", down, options);
    window.addEventListener("pointerup", up, options);
    window.addEventListener("pointercancel", up, options);
    return () => {
      for (const type of ["wheel", "touchstart", "touchmove", "keydown"]) element.removeEventListener(type, mark);
      element.removeEventListener("pointerdown", down);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", up);
    };
  }, []);

  // Images, Markdown, folded activity, fonts and the footer settle after React's layout pass, and a frame can be resized
  // or re-shown: keep following while pinned.
  useEffect(() => {
    const observer = new ResizeObserver(() => { if (pinned.current && !anchor.current) toBottom(); });
    if (scroller.current) observer.observe(scroller.current);
    if (content.current) observer.observe(content.current);
    const shown = () => { if (document.visibilityState === "visible") whenShown(); };
    document.addEventListener("visibilitychange", shown);
    window.addEventListener("pageshow", shown);
    return () => { observer.disconnect(); document.removeEventListener("visibilitychange", shown); window.removeEventListener("pageshow", shown); };
  }, [toBottom, whenShown, view]);

  // After every update: keep the anchor when earlier history was prepended, follow when pinned, otherwise flag news.
  const previousTurns = useRef(turns);
  useLayoutEffect(() => {
    const element = scroller.current;
    if (!element) return;
    const before = previousTurns.current;
    previousTurns.current = turns;
    const page = Boolean(snapshot?.history.page);
    const returnedLatest = previousPage.current && !page;
    previousPage.current = page;
    if (thread?.id && turns.length && (initialThread.current !== thread.id || returnedLatest)) {
      if (initialThread.current !== thread.id) {
        const seen = readSeen(thread.id);
        setMarker(seen && seen !== turns.at(-1)?.id && turns.some((turn) => turn.id === seen) ? { thread: thread.id, after: seen } : null);
      }
      initialThread.current = thread.id;
      anchor.current = null;
      pinned.current = true;
      setAway(false); setFresh(false);
      toBottom();
      return;
    }
    if (turns.length === before.length && turns.every((turn, index) => turn === before[index])) return;
    if (anchor.current) {
      element.scrollTop = anchor.current.top + element.scrollHeight - anchor.current.height;
      remember(element);
      anchor.current = null;
    } else if (pinned.current) toBottom();
    else setFresh(true);
  }, [turns, thread?.id, snapshot?.history.page, view, toBottom]);

  // Remember the latest turn this reader has seen, for the "new since your last visit" divider next time.
  const latest = turns.at(-1)?.id;
  useEffect(() => {
    if (!thread?.id || !latest || snapshot?.history.page || stale) return;
    const save = () => { if (pinned.current && document.visibilityState === "visible") writeSeen(thread.id, latest); };
    save();
    document.addEventListener("visibilitychange", save);
    return () => document.removeEventListener("visibilitychange", save);
  }, [thread?.id, latest, away, snapshot?.history.page, stale]);

  async function older() {
    const element = scroller.current;
    if (element) anchor.current = { height: element.scrollHeight, top: element.scrollTop };
    if (!(await actions.loadOlder())) anchor.current = null; // otherwise the layout effect consumes it
  }
  function jump() {
    const element = scroller.current;
    if (!element) return;
    pinned.current = true;
    element.scrollTo({ top: element.scrollHeight, behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
    setFresh(false); setAway(false);
  }
  const name = snapshot?.harness.name || t("app.name");

  return (
    <div className="threadwrap">
      <div className="thread" id="thread" ref={scroller} tabIndex={-1} data-view={view} onScroll={onScroll} aria-label={t("thread.label")} role="region">
        <div className="col" ref={content}>
          {!state.connected && (snapshot || stale) && (
            <div className="banner warn" role="alert"><Icon name="warn" /><div><b>{t("banner.disconnected")}</b><p>{state.error ?? t("banner.reconnecting")}</p></div><button type="button" className="btn" onClick={actions.retry}>{t("banner.retry")}</button></div>
          )}
          {snapshot && <RecoveryBanner snapshot={snapshot} />}
          {view === "error" && (
            <div className="banner bad" role="alert"><Icon name="warn" /><div><b>{t("banner.unavailable")}</b><p>{state.error}</p></div><button type="button" className="btn" onClick={actions.retry}>{t("banner.retry")}</button></div>
          )}
          {view === "empty" && (
            <div className="empty">
              <h2>{t("empty.title", { name })}</h2>
              <p>{t("empty.text")}</p>
              <ul>
                <li><Icon name="attach" />{t("empty.tipAttach")}</li>
                <li><Icon name="folder" />{t("empty.tipFiles")}</li>
                <li><Icon name="queue" />{t("empty.tipQueue")}</li>
              </ul>
            </div>
          )}
          {view === "loading" && (
            <div className="skeleton" role="status" aria-label={t("loading.label")}><div className="sk w55" /><div className="sk w90" /><div className="sk w96" /><div className="sk w60" /></div>
          )}
          {view === "chat" && (
            <div className={stale ? "chat stale" : "chat"} role="log" aria-label={t("thread.label")} aria-busy={running || undefined}>
              {snapshot?.history.page && <div className="older"><span role="status">{t("thread.olderPage")}</span><button type="button" className="btn" onClick={() => void actions.loadLatest().then(() => scroller.current?.focus({ preventScroll: true }))}>{t("thread.latestPage")}</button></div>}
              {thread?.hasOlderItems && (
                <div className="older"><button type="button" className="btn" disabled={state.loadingOlder} onClick={() => void older()}>{state.loadingOlder ? t("thread.loadingOlder") : t("thread.older")}</button></div>
              )}
              {turns.map((turn, index) => (
                <Fragment key={turn.id}>
                  <TurnView turn={turn} latest={index === turns.length - 1} />
                  {state.notices.filter((notice) => notice.afterTurn === turn.id).map((notice, n) => <GoalDone key={n} objective={notice.objective} />)}
                  {marker?.thread === thread?.id && marker?.after === turn.id && index < turns.length - 1 && <div className="since" role="separator"><span>{t("thread.since")}</span></div>}
                </Fragment>
              ))}
              {state.notices.filter((notice) => !turns.some((turn) => turn.id === notice.afterTurn)).map((notice, n) => <GoalDone key={`x${n}`} objective={notice.objective} />)}
            </div>
          )}
        </div>
      </div>
      {away && view === "chat" && (
        <button type="button" className="jump" data-new={fresh || undefined} onClick={jump}><Icon name="down" /><span>{fresh ? t("jump.new") : t("jump.latest")}</span></button>
      )}
    </div>
  );
}
