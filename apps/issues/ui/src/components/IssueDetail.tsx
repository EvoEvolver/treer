import { Fragment, useEffect, useState } from "react";
import { api } from "../api";
import { useApp } from "../context";
import { PRIORITIES, STATUSES, displayName, words } from "../lib";
import type { ActivityEvent, Issue, IssueDetailResult, IssuePage, Priority, Status } from "../types";
import { Field, PickerField, Select } from "./Fields";
import { IssueLine } from "./IssueList";
import { Markdown, firstLine } from "./Markdown";
import { AssigneePicker } from "./Picker";

const IssueLink = ({ id }: { id: number }) => <a href={"#issue=" + id}>#{id}</a>;

const WAKE_TEXT: Record<string, string> = { prompted: "woken", deduped: "already woken recently", denied: "not woken (Policy)", pending: "waking…", off: "wake-ups off" };

function Delivery({ event }: { event: ActivityEvent }) {
  const { people } = useApp();
  const n = event.notification, wake = Object.entries(n.wake ?? {});
  const failed = ["disabled", "unavailable", "pending"].includes(n.status);
  if (!failed && !wake.length) return null;
  return (
    <p className={"meta delivery" + (failed ? " warning" : "")}>
      {failed && <>Notification {n.status}{n.code ? ": " + n.code : ""}{wake.length ? " · " : ""}</>}
      {wake.map(([agent, outcome], i) => <Fragment key={agent}>{i ? ", " : ""}{displayName(people, agent).split(" · ")[0]} {WAKE_TEXT[outcome] ?? outcome.replace("failed:", "wake failed: ")}</Fragment>)}
    </p>
  );
}

function Comment({ event, issueId, refresh }: { event: ActivityEvent; issueId: number; refresh: () => void }) {
  const { people, mutate, busy } = useApp();
  const [open, setOpen] = useState(false);
  const hidden = !!event.hidden, body = event.body ?? "";
  const headline = event.title || firstLine(body) || "(empty comment)";
  const long = !!event.title || body.trim().includes("\n") || headline !== body.trim();
  const toggleHidden = async () => {
    if (await mutate(`v1/issues/${issueId}/activity/${event.id}${hidden ? "/restore" : ""}`, hidden ? "POST" : "DELETE", undefined, { quiet: true })) refresh();
  };
  return (
    <article className={"comment" + (hidden ? " is-hidden" : "") + (open ? " open" : "")} data-event-id={event.id}>
      <header>
        <button type="button" className="comment-head" aria-expanded={open} disabled={!long && !hidden} onClick={() => setOpen(!open)}>
          <span className="caret">{long || hidden ? (open ? "▾" : "▸") : "·"}</span>
          <span className="comment-headline">{hidden && !body ? "Hidden comment" : headline}</span>
        </button>
        <span className="meta">{displayName(people, event.actor).split(" · ")[0]} · {new Date(event.at).toLocaleString()}</span>
        <button type="button" className="link-button" disabled={busy} onClick={toggleHidden}
            aria-label={(hidden ? "Restore" : "Hide") + ` comment ${event.id}`}>{hidden ? "Restore" : "Hide"}</button>
      </header>
      {hidden && <p className="meta">Hidden by {displayName(people, event.hidden!.by).split(" · ")[0]} · {new Date(event.hidden!.at).toLocaleString()}</p>}
      {open && body && <Markdown text={body} />}
      <Delivery event={event} />
    </article>
  );
}

const CHANGE_TITLES: Record<string, string> = {
  title: "Title", description: "Description", assignee: "Assignee", parent_ids: "Parents", parent_id: "Parent", blocked_by: "Blocked by",
  labels: "Labels", priority: "Priority", status: "Status", comment: "Comment",
};

function SystemEvent({ event }: { event: ActivityEvent }) {
  const { people } = useApp();
  const changes = Object.entries(event.changes ?? {}).filter(([key]) => key !== "version" && key !== "title" && key !== "description");
  const display = (key: string, value: unknown) => key === "assignee" ? displayName(people, value as string | null)
    : Array.isArray(value) ? value.join(", ") || "none" : String(value ?? "none");
  return (
    <article className="system-event">
      <span className="meta">{displayName(people, event.actor).split(" · ")[0]} · {new Date(event.at).toLocaleString()} · {event.summary}</span>
      {changes.length > 0 && <span className="meta"> — {changes.map(([key, value]) => `${CHANGE_TITLES[key] ?? key}: ${display(key, value)}`).join("; ")}</span>}
      <Delivery event={event} />
    </article>
  );
}

// Newest first. Comments only unless "System events" is on; hidden comments are stubs unless "Hidden" is on.
function Timeline({ id, version }: { id: number; version: number }) {
  const { run } = useApp();
  const [system, setSystem] = useState(false), [hidden, setHidden] = useState(false), [tick, setTick] = useState(0);
  const [events, setEvents] = useState<ActivityEvent[] | null>(null), [next, setNext] = useState<number | null>(null);
  const query = (offset: number) => `v1/issues/${id}?` + new URLSearchParams({ offset: String(offset), events: system ? "all" : "comments", ...(hidden ? { hidden: "1" } : {}) });
  useEffect(() => {
    let live = true;
    void run(async () => { const page = await api<IssueDetailResult>(query(0)); if (live) { setEvents(page.activity); setNext(page.next_activity_offset); } });
    return () => { live = false; };
  }, [id, version, system, hidden, tick]);
  return (
    <div className="timeline">
      <div className="timeline-toggles">
        <label><input type="checkbox" checked={system} onChange={(e) => setSystem(e.target.checked)} /> System events</label>
        <label><input type="checkbox" checked={hidden} onChange={(e) => setHidden(e.target.checked)} /> Show hidden text</label>
      </div>
      {events === null ? <p className="empty">Loading…</p> : events.length === 0 ? <p className="empty">No comments yet.</p> : events.map((e) => e.kind === "comment"
        ? <Comment key={e.id} event={e} issueId={id} refresh={() => setTick((t) => t + 1)} /> : <SystemEvent key={e.id} event={e} />)}
      {next !== null && (
        <button type="button" onClick={() => run(async () => {
          const older = await api<IssueDetailResult>(query(next));
          setEvents((prev) => [...(prev ?? []), ...older.activity]);
          setNext(older.next_activity_offset);
        })}>Older</button>
      )}
    </div>
  );
}

function Children({ id, first }: { id: number; first: IssueDetailResult }) {
  const { run } = useApp();
  const [items, setItems] = useState(first.children);
  const [next, setNext] = useState<number | null>(first.children_total > first.children.length ? first.children.length : null);
  if (!items.length) return <p className="empty">No child issues.</p>;
  return (
    <div className="detail-children">
      {items.map((c) => <IssueLine key={c.id} issue={c} />)}
      {next !== null && (
        <button type="button" onClick={() => run(async () => {
          const page = await api<IssuePage>(`v1/issues/${id}/children?limit=100&offset=${next}`);
          setItems((prev) => [...prev, ...page.issues]);
          setNext(page.next_offset);
        })}>More children</button>
      )}
    </div>
  );
}

// Attach EXISTING issues under this one; one issue may sit under several parents.
function AttachChildren({ id }: { id: number }) {
  const { mutate, busy } = useApp();
  const [ids, setIds] = useState("");
  return (
    <form className="attach" onSubmit={async (e) => {
      e.preventDefault();
      const add = ids.split(/[\s,]+/).filter(Boolean).map(Number);
      if (add.length && add.every((n) => Number.isInteger(n) && n > 0) && await mutate(`v1/issues/${id}/children`, "POST", { add })) setIds("");
    }}>
      <input aria-label="Attach existing issues (IDs)" placeholder="Attach existing issues: 12, 15" value={ids} onChange={(e) => setIds(e.target.value)} />
      <button type="submit" disabled={busy || !ids.trim()}>Attach</button>
    </form>
  );
}

function CommentForm({ id }: { id: number }) {
  const { mutate, busy } = useApp();
  const [body, setBody] = useState(""), [title, setTitle] = useState("");
  return (
    <form className="form" onSubmit={async (e) => {
      e.preventDefault();
      const trimmed = title.trim();
      if (await mutate(`v1/issues/${id}/comment`, "POST", trimmed ? { title: trimmed, body } : { body })) { setBody(""); setTitle(""); }
    }}>
      <Field text="Title (optional, one line, up to 64 characters: readers can skim titles and open the text)">
        <input aria-label="Comment title (optional)" maxLength={64} value={title} onChange={(e) => setTitle(e.target.value.replace(/[\r\n]+/g, " "))} />
      </Field>
      <Field text="Comment (Markdown; @name or @/path notifies and wakes an Agent)">
        <textarea aria-label="Comment (Markdown)" required maxLength={8192} value={body} onChange={(e) => setBody(e.target.value)}></textarea>
      </Field>
      <div className="buttons"><button type="submit" disabled={busy}>Post comment</button></div>
    </form>
  );
}

export function IssueDetail({ result, startEdit }: { result: IssueDetailResult; startEdit: (issue: Issue | null, parent?: number | null) => void }) {
  const { people, busy, mutate, closeIssue, reopenIssue, back } = useApp();
  const issue = result.issue, id = issue.id, closed = issue.status === "closed";
  const patch = (body: Record<string, unknown>) => mutate(`v1/issues/${id}`, "PATCH", { ...body, version: issue.version });
  return (
    <>
      <div className="detail-bar">
        <button type="button" className="detail-back" onClick={back}>‹ Back to list</button>
        <span className="meta-id">ISS-{id}</span>
        {(issue.parent_ids ?? []).length > 0 && (
          <span className="meta">in {(issue.parent_ids ?? []).map((p) => (
            <Fragment key={p}><IssueLink id={p} />
              <button type="button" className="unlink" disabled={busy} title={`Remove from #${p}`} aria-label={`Remove from #${p}`}
                onClick={() => mutate(`v1/issues/${p}/children`, "POST", { remove: [id] })}>×</button>{" "}</Fragment>
          ))}</span>
        )}
        <span className="grow"></span>
        <button type="button" className={closed ? "" : "primary"} disabled={busy} onClick={() => (closed ? reopenIssue : closeIssue)(issue)}>{closed ? "Reopen issue" : "Close issue"}</button>
        <button type="button" onClick={() => startEdit(issue)}>Edit issue</button>
        <button type="button" onClick={() => startEdit(null, id)}>Add child</button>
        <button type="button" disabled={busy} onClick={() => mutate(`v1/issues/${id}${issue.hidden ? "/restore" : ""}`, issue.hidden ? "POST" : "DELETE", undefined)}>
          {issue.hidden ? "Restore issue" : "Hide issue"}</button>
      </div>
      <h2>{issue.title}</h2>
      {issue.hidden && <p className="locked">Hidden by {displayName(people, issue.hidden.by)} on {new Date(issue.hidden.at).toLocaleString()}. It is kept with its history; restore it to list it again.</p>}
      <p className="meta">{words(issue.status)} · {issue.priority} · created by {displayName(people, issue.creator)}</p>
      <div className="props">
        <Select<Status> text="Status" values={STATUSES} value={issue.status} disabled={busy} onChange={(status) => patch({ status })} />
        <PickerField text="Assignee">
          <AssigneePicker value={issue.assignee ?? ""} disabled={busy} onChange={(assignee) => patch({ assignee: assignee || null })} />
        </PickerField>
        <Select<Priority> text="Priority" values={PRIORITIES} value={issue.priority} disabled={busy} onChange={(priority) => patch({ priority })} />
      </div>
      {issue.description ? <Markdown text={issue.description} className="markdown description" /> : <p className="description empty">No description.</p>}
      <div>{issue.labels.map((l) => <span key={l} className="tag">{l}</span>)}</div>
      {issue.blocked_by.length > 0 && (
        <p className="meta">Blocked by: {issue.blocked_by.map((d) => <Fragment key={d}><IssueLink id={d} /> </Fragment>)}</p>
      )}
      {(result.referenced_by?.length ?? 0) > 0 && (
        <>
          <h3>Referenced by · {result.referenced_by!.length}</h3>
          <ul className="backlinks">
            {result.referenced_by!.map((link) => (
              <li key={link.issue.id}><IssueLink id={link.issue.id} /> <span className="text">{link.issue.title}</span>
                <span className="meta"> · in its {link.source} by {displayName(people, link.author)}</span></li>
            ))}
          </ul>
        </>
      )}
      <h3>Children · {issue.progress.completed}/{issue.progress.total} descendants complete</h3>
      <AttachChildren id={id} />
      <Children key={"c" + issue.version} id={id} first={result} />
      <h3>Comments</h3>
      <CommentForm id={id} />
      <Timeline id={id} version={issue.version} />
    </>
  );
}
