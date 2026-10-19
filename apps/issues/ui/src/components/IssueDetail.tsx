import { Fragment, useState } from "react";
import { api } from "../api";
import { useApp } from "../context";
import { NOT_ALLOWED, PRIORITIES, STATUSES, canEdit, displayName, words } from "../lib";
import type { ActivityEvent, Issue, IssueDetailResult, IssuePage, Priority, Status } from "../types";
import { Field, PickerField, Select } from "./Fields";
import { IssueLine } from "./IssueList";
import { AssigneePicker } from "./Picker";

const IssueLink = ({ id }: { id: number }) => <a href={"#issue=" + id}>#{id}</a>;

const CHANGE_TITLES: Record<string, string> = {
  title: "Title", description: "Description", assignee: "Assignee", parent_id: "Parent", blocked_by: "Blocked by",
  labels: "Labels", priority: "Priority", status: "Status",
};

function Activity({ event }: { event: ActivityEvent }) {
  const { people } = useApp();
  const changes = Object.entries(event.changes ?? {}).filter(([key]) => key !== "version");
  const display = (key: string, value: unknown) => key === "description" ? "updated" : key === "assignee" ? displayName(people, value as string | null)
    : Array.isArray(value) ? value.join(", ") || "none" : String(value ?? "none");
  return (
    <article className="activity">
      <div className="meta">{displayName(people, event.actor)} · {new Date(event.at).toLocaleString()}</div>
      <p>{event.summary}</p>
      {event.title && event.body ? (
        <details className="comment-titled"><summary>{event.title}</summary><p className="comment-body">{event.body}</p></details>
      ) : (
        <>
          {event.title && <p className="comment-title">{event.title}</p>}
          {event.body && <p className="comment-body">{event.body}</p>}
        </>
      )}
      {changes.map(([key, value]) => <div key={key} className="meta description">{CHANGE_TITLES[key] ?? key}: {display(key, value)}</div>)}
      {["disabled", "unavailable", "pending"].includes(event.notification.status) && (
        <p className="warning">Notification {event.notification.status}{event.notification.code ? ": " + event.notification.code : ""}</p>
      )}
    </article>
  );
}

function Timeline({ id, first }: { id: number; first: IssueDetailResult }) {
  const { run } = useApp();
  const [events, setEvents] = useState(first.activity), [next, setNext] = useState(first.next_activity_offset);
  return (
    <div>
      {events.map((e) => <Activity key={e.id} event={e} />)}
      {next !== null && (
        <button type="button" onClick={() => run(async () => {
          const older = await api<IssueDetailResult>(`v1/issues/${id}?offset=${next}`);
          setEvents((prev) => [...prev, ...older.activity]);
          setNext(older.next_activity_offset);
        })}>Older activity</button>
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
      <Field text="Comment (text / Markdown source)">
        <textarea aria-label="Comment (text / Markdown source)" required maxLength={8192} value={body} onChange={(e) => setBody(e.target.value)}></textarea>
      </Field>
      <div className="buttons"><button type="submit" disabled={busy}>Post comment</button></div>
    </form>
  );
}

export function IssueDetail({ result, startEdit }: { result: IssueDetailResult; startEdit: (issue: Issue | null, parent?: number | null) => void }) {
  const { people, me, busy, mutate, closeIssue, reopenIssue, back } = useApp();
  const issue = result.issue, id = issue.id, claim = issue.claim, allowed = canEdit(me, issue), closed = issue.status === "closed";
  const patch = (body: Record<string, unknown>) => mutate(`v1/issues/${id}`, "PATCH", { ...body, version: issue.version });
  const lease = (action: "acquire" | "renew" | "release") => mutate(`v1/issues/${id}/claim`, "POST", action === "release" ? { action } : { action, seconds: 900 });
  return (
    <>
      <div className="detail-bar">
        <button type="button" className="detail-back" onClick={back}>‹ Back to list</button>
        <span className="meta-id">ISS-{id}</span>
        {issue.parent_id !== null && <span className="meta">in <IssueLink id={issue.parent_id} /></span>}
        <span className="grow"></span>
        <button type="button" className={closed ? "" : "primary"} disabled={busy || !allowed} title={allowed ? "" : NOT_ALLOWED}
            onClick={() => (closed ? reopenIssue : closeIssue)(issue)}>{closed ? "Reopen issue" : "Close issue"}</button>
        <button type="button" onClick={() => startEdit(issue)}>Edit issue</button>
        <button type="button" onClick={() => startEdit(null, id)}>Add child</button>
      </div>
      <h2>{issue.title}</h2>
      {!allowed && <p className="locked">{claim && claim.holder !== me?.id ? "Another principal holds the lease. " : ""}You can comment, but only the creator, assignee, lease holder or a human admin can change or close this issue.</p>}
      <p className="meta">{words(issue.status)} · {issue.priority} · created by {displayName(people, issue.creator)}</p>
      <div className="props">
        <Select<Status> text="Status" values={STATUSES} value={issue.status} disabled={busy || !allowed} onChange={(status) => patch({ status })} />
        <PickerField text="Assignee">
          <AssigneePicker value={issue.assignee ?? ""} disabled={busy || !allowed} onChange={(assignee) => patch({ assignee: assignee || null })} />
        </PickerField>
        {issue.status === "blocked" && (
          <Field text="Waiting on">
            <select aria-label="Waiting on" value={issue.waiting_on ?? ""} disabled={busy || !allowed} onChange={(e) => patch({ waiting_on: e.target.value || null })}>
              <option value="">Nobody in particular</option>
              <option value="leader">The leader</option><option value="user">The user</option>
              {people.filter((p) => p.kind === "agent").map((p) => <option key={p.id} value={p.id}>{displayName(people, p.id)}</option>)}
            </select>
          </Field>
        )}
        <Select<Priority> text="Priority" values={PRIORITIES} value={issue.priority} disabled={busy || !allowed} onChange={(priority) => patch({ priority })} />
      </div>
      {issue.waiting_on && <p className="meta">Waiting on {issue.waiting_on === "leader" ? "the leader" : issue.waiting_on === "user" ? "the user" : displayName(people, issue.waiting_on)}
        {issue.waiting_by ? ` · set by ${displayName(people, issue.waiting_by)}` : ""}{issue.waiting_at ? ` on ${new Date(issue.waiting_at).toLocaleString()}` : ""}</p>}
      <p className="description">{issue.description || "No description."}</p>
      <div>{issue.labels.map((l) => <span key={l} className="tag">{l}</span>)}</div>
      {issue.blocked_by.length > 0 && (
        <p className="meta">Blocked by: {issue.blocked_by.map((d) => <Fragment key={d}><IssueLink id={d} /> </Fragment>)}</p>
      )}
      <div className="actions">
        <button type="button" disabled={busy} onClick={() => lease(claim ? "renew" : "acquire")}>{claim ? "Renew lease" : "Claim for 15 min"}</button>
        {claim && <button type="button" disabled={busy} onClick={() => lease("release")}>Release lease</button>}
      </div>
      {claim && <p className="meta">Lease: {displayName(people, claim.holder)} until {new Date(claim.expires_at).toLocaleString()}</p>}
      <h3>Children · {issue.progress.completed}/{issue.progress.total} descendants complete</h3>
      <Children key={"c" + issue.version} id={id} first={result} />
      <h3>Comments &amp; activity</h3>
      <CommentForm id={id} />
      <Timeline key={"t" + issue.version} id={id} first={result} />
    </>
  );
}
