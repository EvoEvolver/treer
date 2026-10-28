import { memo, useState, type ReactNode } from "react";
import { t, type MessageKey } from "../i18n";
import { useNow } from "../lib/clock";
import { toBlocks } from "../lib/blocks";
import { activityStats, compact, duration, toolState } from "../lib/format";
import { changedFiles, RELAYED, retryText, type ChangedFile } from "../lib/run";
import { useOpen } from "../state/open";
import { useFileNavigation } from "../state/files";
import { useStore } from "../state/store";
import type { HistoryItem, TurnDto } from "../types";
import { Diff } from "./Diff";
import { useDialogs } from "./Dialogs";
import { Icon } from "./Icon";
import { Markdown } from "./Markdown";
import { imageUrl } from "../api";
import { safeImageUrl } from "../lib/url";

function Message({ item }: { item: HistoryItem }) {
  if (!item.text && !item.attachments?.length) return null;
  if (item.kind === "userMessage") {
    const origin = RELAYED.exec(item.text);
    const bodyText = origin ? item.text.slice(origin[0].length) : item.text;
    return (
      <article className={origin ? "msg user relayed" : "msg user"}>
        <div className="who">{origin ? t("message.from", { name: origin[1] ?? "" }) : t("message.you")}</div>
        {item.positionUnavailable && <p className="position-note">{t("message.positionUnavailable")}</p>}
        {!!bodyText.trim() && <Markdown className="body" text={bodyText} />}
        {!!item.attachments?.length && <div className="image-list sent-images">{item.attachments.map((image, index) => <img key={image.id} src={safeImageUrl(imageUrl(image.id))} alt={t("images.preview", { number: index + 1 })} loading="lazy" />)}</div>}
      </article>
    );
  }
  return <article className="msg asst"><Markdown className="body" text={item.text} /></article>;
}

const STEP_ICON = { completed: "check", inProgress: "now", pending: "todo" } as const;
function Plan({ item }: { item: HistoryItem }) {
  const steps = item.steps ?? [];
  const done = steps.filter((step) => step.status === "completed").length;
  return (
    <section className="plan" aria-label={t("plan.title")}>
      <h3>{t("plan.title")} <span className="muted">{done}/{steps.length}</span></h3>
      {item.text && item.text !== "Plan" && <p className="plan-note">{item.text}</p>}
      <ul>
        {steps.map((step, index) => (
          <li key={index} className={step.status === "completed" ? "done" : step.status === "inProgress" ? "cur" : undefined}>
            <Icon name={STEP_ICON[step.status]} /><span>{step.step}</span><span className="sr-only">{t(`plan.${step.status}` as MessageKey)}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}

/** Last lines of a running command's output, so progress is visible without opening anything. */
function tail(text: string, lines = 6): string {
  const all = text.replace(/\s+$/, "").split("\n");
  return all.slice(-lines).join("\n");
}

function ToolRow({ turnId, item, live }: { turnId: string; item: HistoryItem; live: boolean }) {
  const key = `${turnId}:${item.id}`;
  const detail = item.kind === "reasoning" ? item.text : item.detailText && item.detailText !== item.text ? item.detailText : "";
  const [open, toggle] = useOpen(`row:${key}`, false);
  const { openText } = useDialogs();
  const state = toolState(item.status);
  const icon = item.kind === "commandExecution" ? "term" : item.kind === "reasoning" ? "spark" : "tool";
  const label = item.kind === "reasoning" ? (item.text.split("\n", 1)[0] || t("activity.reasoning")) : item.text || item.kind;
  const running = live && state === "running";
  const content: ReactNode = (
    <>
      {running ? <i className="spin" aria-hidden="true" /> : <Icon name={state === "failed" ? "warn" : icon} />}
      {item.kind === "commandExecution" ? <code className="n">{label}</code> : <span className={item.kind === "reasoning" ? "n think" : "n"}>{label}</span>}
      <span className={`st st-${state}`}>{t(`tool.${state}` as MessageKey)}</span>
      {detail && <Icon name="chev" className="chev" />}
    </>
  );
  return (
    <li className={`row${state === "failed" ? " failed" : ""}`}>
      {detail ? <button type="button" className="row-btn" aria-expanded={open} onClick={toggle}>{content}</button> : <div className="row-btn">{content}</div>}
      {running && !open && item.kind === "commandExecution" && detail && <pre className="tail" aria-label={t("tool.liveOutput")}>{tail(detail)}</pre>}
      {detail && open && (
        <div className="row-detail">
          {item.kind === "reasoning" ? <div className="detail"><Markdown text={detail} /></div> : <pre className="detail">{detail}</pre>}
          <button type="button" className="btn small" onClick={() => openText(label, detail)}><Icon name="open" />{t("tool.openText")}</button>
        </div>
      )}
    </li>
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

/** "Ran 3 commands, edited 2 files" in words; counts files, not change events. */
function activitySummary(items: HistoryItem[]): string {
  const stats = activityStats(items);
  const files = new Set(items.flatMap((item) => (item.changes ?? []).map((change) => change.path))).size || stats.changes;
  const parts = [
    stats.commands && t(stats.commands === 1 ? "activity.ran.one" : "activity.ran.other", { count: stats.commands }),
    files && t(files === 1 ? "activity.edited.one" : "activity.edited.other", { count: files }),
    stats.tools && t(stats.tools === 1 ? "activity.tools.one" : "activity.tools.other", { count: stats.tools }),
    stats.steps && t(stats.steps === 1 ? "activity.thought.one" : "activity.thought.other", { count: stats.steps }),
  ].filter(Boolean) as string[];
  const text = parts.join(", ");
  return text ? text.charAt(0).toUpperCase() + text.slice(1) : t("activity.title");
}

const Group = memo(function Group({ turn, items, single, latest }: { turn: TurnDto; items: HistoryItem[]; single: boolean; latest: boolean }) {
  const key = `group:${turn.id}:${items[0]?.id}`;
  const live = latest && turn.status === "inProgress";
  const [open, toggle] = useOpen(key, live);
  const now = useNow(live && single);
  const stats = activityStats(items);
  const elapsed = groupDuration(turn, items, single, now);
  const id = `grp-${turn.id}-${items[0]?.id}`.replace(/[^\w-]/g, "_");
  const visible = items.filter((item) => item.kind !== "fileChange");
  return (
    <section className="grp" data-open={open} data-live={live || undefined}>
      <button type="button" className="sum" aria-expanded={open} aria-controls={id} onClick={toggle}>
        <Icon name="chev" className="chev" />
        <span className="t">{activitySummary(items)}</span>
        {elapsed != null && <span className="m">{duration(elapsed)}</span>}
        {stats.failed > 0 && <span className="bad"><Icon name="warn" />{t("activity.failed", { count: stats.failed })}</span>}
      </button>
      {open && visible.length > 0 && <ul className="rows" id={id}>{visible.map((item) => <ToolRow key={item.id} turnId={turn.id} item={item} live={live} />)}</ul>}
      {open && visible.length === 0 && <p className="muted rows-empty" id={id}>{t("activity.onlyChanges")}</p>}
    </section>
  );
});

function FileRow({ turnId, file }: { turnId: string; file: ChangedFile }) {
  const [open, toggle] = useOpen(`file:${turnId}:${file.path}`, false);
  const files = useFileNavigation();
  const kind = file.kind === "add" ? "A" : file.kind === "delete" ? "D" : "M";
  return (
    <li className="cf" data-open={open}>
      <div className="cf-row">
        <button type="button" className="cf-btn" aria-expanded={open} onClick={toggle}>
          <Icon name="chev" className="chev" />
          <span className={`kind k-${kind}`} title={t(`change.${kind}` as MessageKey)}><span aria-hidden="true">{kind}</span><span className="sr-only">{t(`change.${kind}` as MessageKey)}</span></span>
          <code className="path">{file.path}</code>
          <span className="counts"><span className="add">+{file.added}</span> <span className="del">−{file.removed}</span></span>
        </button>
        {files && file.kind !== "delete" && <button type="button" className="ib small" aria-label={t("change.open", { path: file.path })} title={t("change.open", { path: file.path })} onClick={(event) => files.openFile(file.path, event.currentTarget)}><Icon name="open" /></button>}
      </div>
      {open && file.diffs.map((diff, index) => <Diff key={index} diff={diff} label={file.path} />)}
    </li>
  );
}

function ChangedFiles({ turn }: { turn: TurnDto }) {
  const files = changedFiles(turn.items);
  if (!files.length) return null;
  const added = files.reduce((n, file) => n + file.added, 0), removed = files.reduce((n, file) => n + file.removed, 0);
  return (
    <section className="changes" aria-label={t("change.title")}>
      <h3>{t(files.length === 1 ? "change.count.one" : "change.count.other", { count: files.length })} <span className="counts"><span className="add">+{added}</span> <span className="del">−{removed}</span></span></h3>
      <ul>{files.map((file) => <FileRow key={file.path} turnId={turn.id} file={file} />)}</ul>
    </section>
  );
}

function TurnError({ turn, latest }: { turn: TurnDto; latest: boolean }) {
  const { state, actions } = useStore();
  const [copied, setCopied] = useState(false);
  const retry = latest ? retryText(turn) : null;
  const canRetry = Boolean(retry) && Boolean(state.snapshot?.ready) && state.snapshot?.thread?.status !== "running" && !state.submitting;
  async function copy() {
    try { await navigator.clipboard.writeText(turn.error ?? ""); setCopied(true); setTimeout(() => setCopied(false), 1500); } catch { /* clipboard unavailable */ }
  }
  return (
    <div className="turn-error" role="alert">
      <Icon name="warn" />
      <div className="te-body"><b>{t(turn.status === "interrupted" ? "turn.interrupted" : "turn.failed")}</b><p>{turn.error}</p></div>
      <div className="te-actions">
        {retry && <button type="button" className="btn" disabled={!canRetry} title={t("turn.retryTitle")} onClick={() => void actions.prompt(retry, "steer")}><Icon name="retry" />{t("turn.retry")}</button>}
        <button type="button" className="btn" onClick={() => void copy()}><Icon name="copy" />{copied ? t("dialog.copied") : t("turn.copy")}</button>
      </div>
    </div>
  );
}

function Usage({ turn }: { turn: TurnDto }) {
  const usage = turn.usage;
  const parts = [
    usage?.durationMs != null && <time key="d" dateTime={`PT${Math.round(usage.durationMs / 1000)}S`}>{duration(usage.durationMs)}</time>,
    usage?.inputTokens != null && <span key="i">{t("usage.in", { n: compact(usage.inputTokens) })}</span>,
    usage?.outputTokens != null && <span key="o">{t("usage.out", { n: compact(usage.outputTokens) })}</span>,
    turn.status === "interrupted" && !turn.error && <span key="s">{t("turn.stopped")}</span>,
  ].filter(Boolean);
  return parts.length ? <div className="usage">{parts}</div> : null;
}

export const TurnView = memo(function TurnView({ turn, latest }: { turn: TurnDto; latest: boolean }) {
  const blocks = toBlocks(turn.items);
  const groups = blocks.filter((block) => block.type === "group").length;
  const lastGroup = blocks.map((block) => block.type).lastIndexOf("group");
  return (
    <div className="turn" data-status={turn.status}>
      {blocks.map((block, index) => block.type === "message" ? <Message key={block.item.id} item={block.item} />
        : block.type === "plan" ? <Plan key={block.item.id} item={block.item} />
          : <Group key={`g-${block.items[0]?.id}`} turn={turn} items={block.items} single={groups === 1} latest={latest && index === lastGroup} />)}
      <ChangedFiles turn={turn} />
      {turn.error && <TurnError turn={turn} latest={latest} />}
      <Usage turn={turn} />
    </div>
  );
});
