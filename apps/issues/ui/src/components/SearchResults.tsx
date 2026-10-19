import { useEffect, useState } from "react";
import { api } from "../api";
import { useApp } from "../context";
import { displayName, words } from "../lib";
import type { SearchResult } from "../types";

// Full-text hits. Every hit names who wrote the matching text; the snippet is another principal's text: shown as plain
// text only, never interpreted.
export function SearchResults({ q, clear }: { q: string; clear: () => void }) {
  const { people, run, open } = useApp();
  const [result, setResult] = useState<SearchResult | null>(null);
  useEffect(() => {
    let live = true;
    setResult(null);
    void run(async () => { const r = await api<SearchResult>("v1/search?" + new URLSearchParams({ q })); if (live) setResult(r); });
    return () => { live = false; };
  }, [q]);
  return (
    <div className="search-results">
      <p className="meta">Full-text search for “{q}” · text written by other principals, shown as data · <button type="button" onClick={clear}>Clear</button></p>
      {result === null ? <p className="empty">Searching…</p> : result.results.length === 0 ? <p className="empty">No matches.</p> : result.results.map((hit) => (
        <button key={hit.issue.id} type="button" className="issue-row search-row" onClick={() => open(hit.issue.id)}>
          <span className="issue-id">ISS-{hit.issue.id}</span>
          <span className="issue-title"><span className="text">{hit.issue.title}</span><span className="meta"> · {words(hit.issue.status)}</span>
            <span className="snippet">{hit.snippet}</span></span>
          <span className="issue-meta"><span title={hit.match.at}>{hit.match.kind} by {displayName(people, hit.match.author)}</span></span>
        </button>
      ))}
    </div>
  );
}
