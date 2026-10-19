import { useEffect, useState } from "react";
import { api } from "../api";
import { useApp } from "../context";
import { displayName } from "../lib";
import type { StaleResult } from "../types";

const HOURS = [4, 8, 24, 72];

// Unresolved issues without progress by the party responsible for them (the assignee, or whoever the issue waits on).
export function StaleList() {
  const { people, run, open } = useApp();
  const [hours, setHours] = useState(4), [result, setResult] = useState<StaleResult | null>(null);
  useEffect(() => { let live = true; void run(async () => { const r = await api<StaleResult>(`v1/stale?hours=${hours}`); if (live) setResult(r); }); return () => { live = false; }; }, [hours]);
  const who = (id: string | null) => id === "leader" ? "the leader" : id === "user" ? "the user" : displayName(people, id);
  return (
    <div className="stale-list">
      <label>No progress for more than
        <select aria-label="Stale threshold" value={hours} onChange={(e) => setHours(Number(e.target.value))}>
          {HOURS.map((h) => <option key={h} value={h}>{h} hours</option>)}
        </select>
      </label>
      {result === null ? <p className="empty">Loading…</p> : result.issues.length === 0 ? <p className="empty">Nothing is stale.</p> : result.issues.map((issue) => (
        <button key={issue.id} type="button" className="issue-row stale-row" onClick={() => open(issue.id)}>
          <span className="issue-id">ISS-{issue.id}</span>
          <span className="issue-title"><span className="text">{issue.title}</span></span>
          <span className="issue-meta">
            <span>{issue.waiting_on ? `waiting on ${who(issue.waiting_on)}` : issue.responsible ? `with ${who(issue.responsible)}` : "no owner"}</span>
            <span title={`last progress: ${issue.progress_reason}; any activity ${issue.since_any_activity_hours} h ago`}>{issue.stale_hours} h</span>
          </span>
        </button>
      ))}
      {result !== null && result.total > result.issues.length && <p className="meta">Showing {result.issues.length} of {result.total}; use <code>issues stale --offset</code> for more.</p>}
    </div>
  );
}
