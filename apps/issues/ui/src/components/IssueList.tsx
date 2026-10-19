import { Fragment } from "react";
import { useApp } from "../context";
import { NOT_ALLOWED, PRIORITY_GLYPH, STATUSES, canEdit, displayName, words } from "../lib";
import type { Issue, Route, Status } from "../types";
import { Avatar } from "./Avatar";

export type View = "list" | "tree" | "stale";

export function IssueLine({ issue, selected = false, focused = false }: { issue: Issue; selected?: boolean; focused?: boolean }) {
  const { people, me, open, closeIssue, reopenIssue, busy } = useApp();
  const person = people.find((p) => p.id === issue.assignee);
  const closed = issue.status === "closed", allowed = canEdit(me, issue);
  const act = closed ? reopenIssue : closeIssue;
  return (
    <div className={"issue-line" + (selected ? " selected" : "") + (focused ? " kb" : "")} data-issue-id={issue.id}>
      <button type="button" className="issue-row" onClick={() => open(issue.id)}
          aria-label={`ISS-${issue.id} ${issue.title} · ${words(issue.status)} · ${displayName(people, issue.assignee)}`}>
        <span className={"st st-" + issue.status} title={words(issue.status)}></span>
        <span className="issue-id">ISS-{issue.id}</span>
        <span className="issue-title">
          <span className={"text" + (issue.status === "done" || closed ? " done" : "")}>{issue.title}</span>
          {issue.labels.map((l) => <span key={l} className="tag">{l}</span>)}
        </span>
        <span className="issue-meta">
          {issue.progress.total > 0 && <span className="progress" title="Descendants complete">{issue.progress.completed}/{issue.progress.total}</span>}
          {issue.claim && <span title="Leased">⚑</span>}
          <span className={"priority " + issue.priority} title={issue.priority + " priority"}>{issue.priority === "normal" ? "" : PRIORITY_GLYPH[issue.priority]}</span>
          <span title={displayName(people, issue.assignee)}><Avatar person={person} /></span>
        </span>
      </button>
      <button type="button" className="row-close" disabled={busy || !allowed} onClick={() => act(issue)}
          aria-label={(closed ? "Reopen" : "Close") + ` ISS-${issue.id}`}
          title={allowed ? (closed ? "Reopen issue" : "Close issue (X)") : NOT_ALLOWED}>{closed ? "Reopen" : "Close"}</button>
    </div>
  );
}

function TreeBranch({ issue, all, route, focusId }: { issue: Issue; all: Issue[]; route: Route; focusId: number | null }) {
  const children = all.filter((i) => i.parent_id === issue.id);
  return (
    <div>
      <IssueLine issue={issue} selected={route.kind === "issue" && route.id === issue.id} focused={focusId === issue.id} />
      {children.length > 0 && (
        <div className="tree-group">{children.map((c) => <TreeBranch key={c.id} issue={c} all={all} route={route} focusId={focusId} />)}</div>
      )}
    </div>
  );
}

// Visible rows in display order, shared by rendering and j/k navigation.
export function layout(visible: Issue[], view: View, collapsed: ReadonlySet<Status>, filterStatus: string) {
  if (view === "tree") {
    const ids = new Set(visible.map((i) => i.id));
    const order: number[] = [];
    const walk = (issue: Issue) => { order.push(issue.id); visible.filter((i) => i.parent_id === issue.id).forEach(walk); };
    const roots = visible.filter((i) => i.parent_id === null || !ids.has(i.parent_id));
    roots.forEach(walk);
    return { roots, groups: [], order };
  }
  const groups = STATUSES.map((status) => ({ status, items: visible.filter((i) => i.status === status) })).filter((g) => g.items.length);
  const expanded = (status: Status) => filterStatus !== "" || !collapsed.has(status);
  return { roots: [], groups, order: groups.filter((g) => expanded(g.status)).flatMap((g) => g.items.map((i) => i.id)) };
}

export function IssueList({ visible, view, collapsed, toggle, filterStatus, route, focusId }: {
  visible: Issue[]; view: View; collapsed: ReadonlySet<Status>; toggle: (status: Status) => void;
  filterStatus: string; route: Route; focusId: number | null;
}) {
  if (!visible.length) return <p className="empty">No issues match. Create an issue to begin.</p>;
  const { groups, roots } = layout(visible, view, collapsed, filterStatus);
  if (view === "tree") return <>{roots.map((i) => <TreeBranch key={i.id} issue={i} all={visible} route={route} focusId={focusId} />)}</>;
  return (
    <>
      {groups.map(({ status, items }) => {
        const isOpen = filterStatus !== "" || !collapsed.has(status);
        return (
          <Fragment key={status}>
            <button type="button" className="status-group" aria-expanded={isOpen} onClick={() => toggle(status)}>
              <span className="caret">{isOpen ? "▾" : "▸"}</span><span className={"st st-" + status}></span>
              <strong>{words(status)}</strong><span className="count">{items.length}</span>
            </button>
            {isOpen && items.map((i) => <IssueLine key={i.id} issue={i} selected={route.kind === "issue" && route.id === i.id} focused={focusId === i.id} />)}
          </Fragment>
        );
      })}
    </>
  );
}
