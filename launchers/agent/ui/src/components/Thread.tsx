import { Fragment, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { t } from "../i18n";
import { duration } from "../lib/format";
import { useStore } from "../state/store";
import type { Snapshot, ThreadState } from "../types";
import { Icon } from "./Icon";
import { TurnView } from "./Timeline";

const NEAR = 48; // px from the bottom that counts as "at the latest message"
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

/** The conversation. It opens at the latest message and follows new content while the reader is there; scrolling up stops following. */
export function Thread() {
  const { state, actions } = useStore();
  const { snapshot } = state;
  const stale = Boolean(state.staleThread) && (!snapshot?.thread || (!snapshot.ready && snapshot.thread.turns.length === 0));
  const thread: ThreadState | null = stale ? state.staleThread ?? null : snapshot?.thread ?? null;
  const scroller = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  const lastTop = useRef(0);
  const anchor = useRef<{ height: number; top: number } | null>(null);
  const initialThread = useRef<string | null>(null);
  const previousPage = useRef(false);
  const [away, setAway] = useState(false);
  const [fresh, setFresh] = useState(false);
  const [marker, setMarker] = useState<{ thread: string; after: string } | null>(null);
  const turns = thread?.turns ?? [];
  const running = thread?.status === "running";
  const view = !snapshot && !stale ? (state.error ? "error" : "loading") : turns.length === 0 ? (stale ? "loading" : "empty") : "chat";

  const toBottom = useCallback(() => {
    const element = scroller.current;
    if (!element) return;
    element.scrollTop = element.scrollHeight;
    lastTop.current = element.scrollTop;
  }, []);

  // Only a reader moving up unpins; content growing below never does.
  const onScroll = useCallback(() => {
    const element = scroller.current;
    if (!element) return;
    const distance = element.scrollHeight - element.scrollTop - element.clientHeight;
    if (distance < NEAR) pinned.current = true;
    else if (element.scrollTop < lastTop.current - 2) pinned.current = false;
    lastTop.current = element.scrollTop;
    setAway(!pinned.current);
    if (pinned.current) setFresh(false);
  }, []);

  // Images, Markdown, folded activity and the footer settle after React's layout pass: keep following while pinned.
  useEffect(() => {
    const observer = new ResizeObserver(() => { if (pinned.current && !anchor.current) toBottom(); });
    if (scroller.current) observer.observe(scroller.current);
    if (content.current) observer.observe(content.current);
    return () => observer.disconnect();
  }, [toBottom, view]);

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
      lastTop.current = element.scrollTop;
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
