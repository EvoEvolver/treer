import { useEffect, useRef } from "react";
import { t, type MessageKey } from "../i18n";
import { useAppearance } from "../appearance";
import { useStore } from "../state/store";
import type { HistoryItem, TurnDto } from "../types";
import { Icon } from "./Icon";

const FROM = /^\[来自 ([^\r\n]+)，排队于 ([^\r\n]+)\]\n/;

function Item({ item }: { item: HistoryItem }) {
  if (item.kind === "userMessage") {
    const origin = FROM.exec(item.text);
    return (
      <article className="msg user">
        <div className="who">{origin ? t("message.from", { name: origin[1] ?? "" }) : t("message.you")}</div>
        <div className="body plain">{origin ? item.text.slice(origin[0].length) : item.text}</div>
      </article>
    );
  }
  if (item.kind === "agentMessage") return <article className="msg asst"><div className="body plain">{item.text}</div></article>;
  return null;
}

function Turn({ turn }: { turn: TurnDto }) {
  const activity = turn.items.filter((item) => item.kind !== "userMessage" && item.kind !== "agentMessage").length;
  return (
    <>
      {turn.items.map((item) => <Item key={item.id} item={item} />)}
      {activity > 0 && <div className="grp"><div className="sum"><span className="t">{t("activity.items", { count: activity })}</span></div></div>}
      {turn.error && <div className="turn-error" role="alert"><Icon name="warn" /><span>{turn.error}</span></div>}
    </>
  );
}

/** Stage-1 conversation: plain messages only. Stage 2 replaces this with the full timeline (markdown, activity, diff, paging). */
export function Thread() {
  const { state, actions } = useStore();
  const { look } = useAppearance();
  const { snapshot } = state;
  const ref = useRef<HTMLDivElement>(null);
  const turns = snapshot?.thread?.turns ?? [];
  const view = !snapshot ? (state.error ? "error" : "loading") : !state.connected ? "error" : turns.length === 0 ? "empty" : "chat";
  const last = turns.at(-1)?.items.at(-1)?.text.length ?? 0;
  useEffect(() => {
    const element = ref.current;
    if (element && element.scrollHeight - element.scrollTop - element.clientHeight < 160) element.scrollTop = element.scrollHeight;
  }, [turns.length, last]);
  return (
    <div className="thread" id="thread" ref={ref} role="log" aria-live="polite" aria-label={t("thread.label")} tabIndex={-1} data-view={view}>
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
        <div className="chat">{turns.map((turn) => <Turn key={turn.id} turn={turn} />)}</div>
      </div>
    </div>
  );
}
