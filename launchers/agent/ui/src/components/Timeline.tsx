import { memo, type ReactNode } from "react";
import { t, type MessageKey } from "../i18n";
import { useNow } from "../lib/clock";
import { toBlocks } from "../lib/blocks";
import { activityStats, compact, duration, plural, toolState } from "../lib/format";
import { useOpen } from "../state/open";
import type { HistoryItem, TurnDto } from "../types";
import { Diff } from "./Diff";
import { useDialogs } from "./Dialogs";
import { Icon } from "./Icon";
import { Markdown } from "./Markdown";
import { imageUrl } from "../api";
import { safeImageUrl } from "../lib/url";

const FROM = /^\[来自 ([^\r\n]+)，排队于 ([^\r\n]+)\]\n/;

function Message({ item }: { item: HistoryItem }) {
  if (!item.text && !item.attachments?.length) return null;
  if (item.kind === "userMessage") {
    const origin = FROM.exec(item.text);
    const bodyText = origin ? item.text.slice(origin[0].length) : item.text;
    return (
      <article className={`msg user${bodyText.trim() ? "" : " image-only"}`}>
        <div className="who">{origin ? t("message.from", { name: origin[1] ?? "" }) : t("message.you")}</div>
        {!!bodyText.trim() && <Markdown className="body" text={bodyText} />}
        {!!item.attachments?.length && <div className="image-list sent-images">{item.attachments.map((image, index) => <img key={image.id} src={safeImageUrl(imageUrl(image.id))} alt={t("images.preview", { number: index + 1 })} loading="lazy" />)}</div>}
      </article>
    );
  }
  return <article className="msg asst"><Markdown className="body" text={item.text} /></article>;
}

function Plan({ item }: { item: HistoryItem }) {
  const steps = item.steps ?? [];
  const icon = { completed: "check", inProgress: "now", pending: "todo" } as const;
  return (
    <section className="plan" aria-label={t("plan.title")}>
      <h3>{t("plan.title")}</h3>
      {item.text && item.text !== "Plan" && <p className="plan-note">{item.text}</p>}
      <ul>
        {steps.map((step, index) => (
          <li key={index} className={step.status === "completed" ? "done" : step.status === "inProgress" ? "cur" : undefined}>
            <Icon name={icon[step.status]} /><span>{step.step}</span><span className="sr-only">{t(`plan.${step.status}` as MessageKey)}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}

function ToolRow({ turnId, item }: { turnId: string; item: HistoryItem }) {
  const key = `${turnId}:${item.id}`;
  const detail = item.kind === "reasoning" ? item.text : item.detailText && item.detailText !== item.text ? item.detailText : "";
  const [open, toggle] = useOpen(`row:${key}`, false);
  const { openText } = useDialogs();
  const state = toolState(item.status);
  const icon = item.kind === "commandExecution" ? "term" : item.kind === "fileChange" ? "edit" : item.kind === "reasoning" ? "spark" : "term";
  const label = item.kind === "reasoning" ? t("activity.reasoning") : item.kind === "fileChange" && item.changes?.length ? item.changes.map((change) => change.path).join(", ") : item.text || item.kind;
  const content: ReactNode = (
    <>
      <Icon name={icon} />
      {item.kind === "commandExecution" || item.kind === "fileChange" ? <code className="n">{label}</code> : <span className={item.kind === "reasoning" ? "n think-label" : "n"}>{label}</span>}
      <span className="st">{t(`tool.${state}` as MessageKey)}</span>
    </>
  );
  return (
    <>
      <div className={`tool${state === "failed" ? " failed" : ""}${item.kind === "reasoning" ? " think" : ""}`}>
        {detail ? <button type="button" className="tool-btn" aria-expanded={open} onClick={toggle}>{content}</button> : content}
      </div>
      {detail && open && (
        <div className="tool-detail-wrap">
          {item.kind === "reasoning" ? <div className="tool-detail"><Markdown text={detail} /></div> : <pre className="tool-detail">{detail}</pre>}
          <button type="button" className="btn tool-open" onClick={() => openText(label, detail)}>{t("tool.openText")}</button>
        </div>
      )}
      {item.kind === "fileChange" && (item.changes?.length ? item.changes.map((change, index) => <Diff key={index} change={change} />) : <p className="file-notice">{t("diff.noFiles")}</p>)}
    </>
  );
}

/** Elapsed time is shown only when the data supports it: per-item timestamps, or a turn that is exactly one group. */
function groupDuration(turn: TurnDto, items: HistoryItem[], single: boolean, now: number): number | null {
  const first = items[0], last = items.at(-1);
  const start = first?.startedAt ? Date.parse(first.startedAt) : NaN;
  const end = last?.endedAt ? Date.parse(last.endedAt) : NaN;
  if (Number.isFinite(start) && Number.isFinite(end)) return end - start;
  if (!single || !turn.startedAt) return null;
  const turnStart = Date.parse(turn.startedAt);
  if (!Number.isFinite(turnStart)) return null;
  if (turn.endedAt && Number.isFinite(Date.parse(turn.endedAt))) return Date.parse(turn.endedAt) - turnStart;
  return turn.status === "inProgress" ? now - turnStart : null;
}

const Group = memo(function Group({ turn, items, single, latest }: { turn: TurnDto; items: HistoryItem[]; single: boolean; latest: boolean }) {
  const key = `group:${turn.id}:${items[0]?.id}`;
  const live = latest && turn.status === "inProgress";
  const [open, toggle] = useOpen(key, live);
  const now = useNow(live && single);
  const stats = activityStats(items);
  const elapsed = groupDuration(turn, items, single, now);
  const meta = [stats.commands && plural("command", stats.commands), stats.changes && plural("change", stats.changes), stats.tools && plural("tool", stats.tools), stats.steps && plural("step", stats.steps)].filter(Boolean).join(" · ");
  const id = `grp-${turn.id}-${items[0]?.id}`.replace(/[^\w-]/g, "_");
  return (
    <section className="grp" data-open={open} data-warn={stats.failed > 0 || undefined}>
      <button type="button" className="sum" aria-expanded={open} aria-controls={id} onClick={toggle}>
        <Icon name="chev" className="chev" />
        <span className="t">{elapsed != null ? t("activity.worked", { time: duration(elapsed) }) : t("activity.title")}</span>
        <span className="m">{meta}{stats.failed > 0 && <>{meta && " · "}<b className="bad">{t("activity.failed", { count: stats.failed })}</b></>}</span>
      </button>
      <div className="grp-body" id={id}><div className="grp-in">{items.map((item) => <ToolRow key={item.id} turnId={turn.id} item={item} />)}</div></div>
    </section>
  );
});

function Usage({ turn }: { turn: TurnDto }) {
  const usage = turn.usage;
  if (!usage) return null;
  const parts = [
    usage.durationMs != null && <time key="d" dateTime={`PT${Math.round(usage.durationMs / 1000)}S`}>{duration(usage.durationMs)}</time>,
    usage.inputTokens != null && <span key="i">{t("usage.in", { n: compact(usage.inputTokens) })}</span>,
    usage.outputTokens != null && <span key="o">{t("usage.out", { n: compact(usage.outputTokens) })}</span>,
  ].filter(Boolean);
  return parts.length ? <div className="usage">{parts}</div> : null;
}

export const TurnView = memo(function TurnView({ turn, latest }: { turn: TurnDto; latest: boolean }) {
  const blocks = toBlocks(turn.items);
  const groups = blocks.filter((block) => block.type === "group").length;
  const lastGroup = blocks.map((block) => block.type).lastIndexOf("group");
  return (
    <>
      {blocks.map((block, index) => block.type === "message" ? <Message key={block.item.id} item={block.item} />
        : block.type === "plan" ? <Plan key={block.item.id} item={block.item} />
          : <Group key={`g-${block.items[0]?.id}`} turn={turn} items={block.items} single={groups === 1} latest={latest && index === lastGroup} />)}
      <Usage turn={turn} />
      {turn.error && <div className="turn-error" role="alert"><Icon name="warn" /><span>{turn.error}</span></div>}
    </>
  );
});
