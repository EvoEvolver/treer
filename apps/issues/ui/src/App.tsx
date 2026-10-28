import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, ApiError } from "./api";
import { Help } from "./components/Help";
import { IssueDetail } from "./components/IssueDetail";
import { IssueForm } from "./components/IssueForm";
import { IssueList, layout, type View } from "./components/IssueList";
import { SearchResults } from "./components/SearchResults";
import { AssigneePicker } from "./components/Picker";
import { Ctx, type AppContext } from "./context";
import { COLLAPSED_BY_DEFAULT, STATUSES, issueFromHash } from "./lib";
import type { Issue, IssueDetailResult, IssuePage, MutationResult, Principal, Route, Toast, ToastAction } from "./types";

const NAV = {
  "nav-all": { title: "All issues", glyph: "◉", mine: false, unresolved: false },
  "nav-mine": { title: "Mine", glyph: "◎", mine: true, unresolved: true },
  "nav-open": { title: "Open", glyph: "◐", mine: false, unresolved: true },
} as const;
type NavId = keyof typeof NAV;
interface Filters { status: string; assignee: string; label: string; unresolved: boolean; hidden: boolean }

const resultNotice = (result: MutationResult) => {
  const n = result.notification;
  const base = n?.message ? `${n.message} (${n.status})` : "Saved. Notifications: " + (n?.status ?? "best effort");
  // An @name that matched nobody (or several Agents) notified no one: say so, and for a duplicate name offer
  // the exact spellings that would work.
  const missed = (n?.unresolved_mentions ?? []).map((m) => {
    const options = n?.mention_candidates?.[m];
    return options?.length ? `@${m} (ambiguous: ${options.map((o) => "@" + o).join(" or ")})` : `@${m}`;
  });
  const woken = Object.entries(n?.wake ?? {}).filter(([, outcome]) => outcome === "prompted" || outcome === "pending").length;
  return [missed.length ? `${base} · not notified: ${missed.join(", ")}` : base, ...(woken ? [`${woken} Agent${woken > 1 ? "s" : ""} woken`] : [])].join(" · ");
};

export function App() {
  const [me, setMe] = useState<Principal | null>(null), [people, setPeople] = useState<Principal[]>([]);
  const [ready, setReady] = useState(false), [needLogin, setNeedLogin] = useState(false);
  const [items, setItems] = useState<Issue[]>([]), [hasMore, setHasMore] = useState(false);
  const [nav, setNav] = useState<NavId>("nav-all"), [view, setView] = useState<View>("list");
  const [filters, setFilters] = useState<Filters>({ status: "", assignee: "", label: "", unresolved: false, hidden: false });
  const [labelDraft, setLabelDraft] = useState(""), [search, setSearch] = useState(""), [deep, setDeep] = useState<string | null>(null);
  const [collapsed, setCollapsed] = useState<Set<(typeof STATUSES)[number]>>(new Set(COLLAPSED_BY_DEFAULT));
  const [route, setRoute] = useState<Route>(() => { const id = issueFromHash(); return id ? { kind: "issue", id } : { kind: "none" }; });
  const [detail, setDetail] = useState<IssueDetailResult | null>(null), [reloadKey, setReloadKey] = useState(0), [busy, setBusy] = useState(false);
  const [focusId, setFocusId] = useState<number | null>(null), [helpOpen, setHelpOpen] = useState(false);
  const [toast, setToast] = useState<Toast>({ id: 0, text: "Connecting to workspace…" });
  const [theme, setTheme] = useState(() => document.documentElement.dataset.theme === "dark" ? "dark" : "light");
  const nextOffset = useRef<number | null>(0), filtersRef = useRef(filters), loadSeq = useRef(0), searchRef = useRef<HTMLInputElement>(null);
  filtersRef.current = filters;

  const notify = useCallback((text: string, action?: ToastAction) => setToast((t) => ({ id: t.id + 1, text, action })), []);
  const failure = useCallback((e: unknown) => {
    notify(e instanceof Error ? e.message : "Request failed");
    if (e instanceof ApiError && e.status === 401 && e.code === "authentication_required") setNeedLogin(true);
  }, [notify]);
  const run = useCallback(async <T,>(fn: () => Promise<T>) => { try { return await fn(); } catch (e) { failure(e); } }, [failure]);

  const load = useCallback(async (reset = true) => {
    const params = new URLSearchParams({ limit: "100", offset: reset ? "0" : String(nextOffset.current ?? 0) });
    for (const key of ["status", "assignee", "label"] as const) if (filtersRef.current[key]) params.set(key, filtersRef.current[key]);
    if (filtersRef.current.unresolved) params.set("unresolved", "1");
    if (filtersRef.current.hidden) params.set("hidden", "1");
    const seq = ++loadSeq.current;
    const result = await api<IssuePage>("v1/issues?" + params);
    if (seq !== loadSeq.current) return;
    setItems((prev) => reset ? result.issues : [...prev, ...result.issues]);
    nextOffset.current = result.next_offset;
    setHasMore(result.next_offset !== null);
  }, []);
  const reload = useCallback(() => { run(() => load(true)); setReloadKey((k) => k + 1); }, [load, run]);

  const mutate = useCallback<AppContext["mutate"]>(async (path, method, body, { quiet = false } = {}) => {
    setBusy(true);
    try {
      const result = await api<MutationResult>(path, method, body);
      await load(true);
      setReloadKey((k) => k + 1);
      if (!quiet) notify(resultNotice(result));
      return result;
    } catch (e) {
      failure(e);
      load(true).catch(() => {});
      setReloadKey((k) => k + 1);
      return null;
    } finally { setBusy(false); }
  }, [load, notify, failure]);

  const setStatus = useCallback(async (issue: Issue, status: Issue["status"], label: string, undo: boolean): Promise<void> => {
    const result = await mutate(`v1/issues/${issue.id}`, "PATCH", { status, version: issue.version }, { quiet: true });
    if (result) notify(`${label} ISS-${issue.id}`, undo ? { label: "Undo", run: () => { void setStatus(result.issue, issue.status, "Restored", false); } } : undefined);
  }, [mutate, notify]);
  const closeIssue = useCallback((issue: Issue) => setStatus(issue, "closed", "Closed", true), [setStatus]);
  const reopenIssue = useCallback((issue: Issue) => setStatus(issue, "open", "Reopened", false), [setStatus]);

  const open = useCallback((id: number) => { history.pushState(null, "", `#issue=${id}`); setFocusId(id); setRoute({ kind: "issue", id }); }, []);
  const back = useCallback(() => { history.replaceState(null, "", location.pathname + location.search); setRoute({ kind: "none" }); }, []);
  const startEdit = (issue: Issue | null, parent: number | null = null) => setRoute(issue ? { kind: "edit", issue } : { kind: "new", parent });
  const finishForm = (result: MutationResult) => {
    notify(resultNotice(result));
    history.pushState(null, "", `#issue=${result.issue.id}`);
    setRoute({ kind: "issue", id: result.issue.id });
    reload();
  };

  // Boot: identity, directory, then the first page.
  useEffect(() => { void run(async () => {
    const identity = await api<{ principal: Principal }>("v1/me");
    setMe(identity.principal);
    const directory = await api<{ principals: Principal[]; paths?: string }>("v1/directory");
    setPeople(directory.principals);
    setReady(true);
    notify("Assign an Agent or @mention it to notify and wake it; delivery is shown on each activity entry."
      + (directory.paths === "supported" ? "" : " This Treer Proxy does not report Agent tree paths yet; Agents are shown by stable ID."));
  }); }, []);
  useEffect(() => { if (ready) void run(() => load(true)); }, [ready, filters]);
  useEffect(() => {
    if (route.kind !== "issue") { setDetail(null); return; }
    let live = true;
    void run(async () => { const result = await api<IssueDetailResult>(`v1/issues/${route.id}`); if (live) setDetail(result); });
    return () => { live = false; };
  }, [route.kind, route.kind === "issue" ? route.id : 0, reloadKey]);
  useEffect(() => {
    const onHash = () => { const id = issueFromHash(); setRoute(id ? { kind: "issue", id } : { kind: "none" }); };
    addEventListener("hashchange", onHash);
    return () => removeEventListener("hashchange", onHash);
  }, []);
  useEffect(() => { document.body.classList.toggle("has-detail", route.kind !== "none"); }, [route.kind]);
  useEffect(() => { document.documentElement.dataset.theme = theme; }, [theme]);
  useEffect(() => {
    if (!toast.text) return;
    const timer = setTimeout(() => setToast((t) => t.id === toast.id ? { id: t.id, text: "" } : t), toast.action ? 8000 : 6000);
    return () => clearTimeout(timer);
  }, [toast.id]);

  const query = search.trim().toLowerCase();
  const visible = useMemo(() => items.filter((i) => !query || `${i.title} ISS-${i.id} ${i.labels.join(" ")}`.toLowerCase().includes(query)), [items, query]);
  const { order } = useMemo(() => layout(visible, view, collapsed, filters.status), [visible, view, collapsed, filters.status]);

  useEffect(() => { document.querySelector(".issue-line.kb")?.scrollIntoView({ block: "nearest" }); }, [focusId]);
  // Keyboard-first: j/k move, Enter opens, x closes, e edits, c creates, / searches, ? helps.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement;
      if (event.ctrlKey || event.metaKey || event.altKey || target.closest("input,textarea,select,[contenteditable=true],.picker-popup")) return;
      const key = event.key;
      if (helpOpen) { if (key === "Escape" || key === "?") { event.preventDefault(); setHelpOpen(false); } return; }
      if (key === "?") { event.preventDefault(); setHelpOpen(true); return; }
      if (!ready) return;
      const focused = items.find((i) => i.id === focusId);
      if (key === "c") { event.preventDefault(); startEdit(null); }
      else if (key === "/") { event.preventDefault(); searchRef.current?.focus(); }
      else if ((key === "j" || key === "k" || key === "ArrowDown" || key === "ArrowUp") && !target.closest("#detail")) {
        event.preventDefault();
        const step = key === "j" || key === "ArrowDown" ? 1 : -1, at = order.indexOf(focusId ?? -1);
        setFocusId(order[Math.min(order.length - 1, Math.max(0, at < 0 ? (step > 0 ? 0 : order.length - 1) : at + step))] ?? null);
      }
      else if ((key === "Enter" || key === "o") && focused && !target.closest(".issue-line")) { event.preventDefault(); open(focused.id); }
      else if (key === "x" && focused) {
        event.preventDefault();
        if (focused.status !== "closed") void closeIssue(focused);
      }
      else if (key === "e" && route.kind === "issue" && detail?.issue.id === route.id) { event.preventDefault(); startEdit(detail.issue); }
      else if (key === "Escape" && route.kind !== "none") back();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  });

  const applyNav = (id: NavId) => {
    const spec = NAV[id];
    setNav(id);
    setFilters((f) => ({ status: "", assignee: spec.mine && me ? me.id : "", label: "", unresolved: spec.unresolved, hidden: f.hidden }));
    setLabelDraft("");
    setDeep(null);
  };
  const commitLabel = () => setFilters((f) => f.label === labelDraft.trim() ? f : { ...f, label: labelDraft.trim() });
  const toggleGroup = (status: (typeof STATUSES)[number]) => setCollapsed((prev) => { const next = new Set(prev); if (!next.delete(status)) next.add(status); return next; });
  const toggleTheme = () => {
    const next = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
    try { localStorage.setItem("issues-theme", next); } catch { /* storage unavailable */ }
    setTheme(next);
  };

  const ctx: AppContext = { me, people, busy, mutate, run, notify, open, back, reload, closeIssue, reopenIssue };
  let pane;
  if (route.kind === "new") pane = <IssueForm key={"new" + route.parent} parent={route.parent} cancel={back} done={finishForm} />;
  else if (route.kind === "edit") { const issue = route.issue; pane = <IssueForm key={"edit" + issue.id} issue={issue} cancel={() => setRoute({ kind: "issue", id: issue.id })} done={finishForm} />; }
  else if (route.kind === "issue") pane = detail?.issue.id === route.id ? <IssueDetail result={detail} startEdit={startEdit} /> : <p className="empty">Loading…</p>;
  else pane = (
    <div className="detail-placeholder"><h2>No issue selected</h2><p>Pick an issue to see details, or press <kbd>C</kbd> to create one.</p>
      <p className="hint"><kbd>J</kbd> <kbd>K</kbd> move · <kbd>↵</kbd> open · <kbd>X</kbd> close · <kbd>?</kbd> all shortcuts</p></div>
  );

  return (
    <Ctx.Provider value={ctx}>
      <aside className="sidebar">
        <a className="brand" href="./"><span className="brand-mark">T</span><span className="brand-text">Treer<small>Issues</small></span></a>
        <nav className="nav" aria-label="Views">
          {(Object.keys(NAV) as NavId[]).map((id) => (
            <button key={id} id={id} type="button" className={"nav-link" + (nav === id ? " active" : "")} disabled={!ready} onClick={() => applyNav(id)}>
              <span className="nav-glyph">{NAV[id].glyph}</span><span className="nav-text">{NAV[id].title}</span>
              {id === "nav-all" && <span id="issue-count" className="nav-count">{items.length}</span>}
            </button>
          ))}
        </nav>
        <div className="sidebar-footer">
          <span id="identity">{me ? me.name : "Connecting…"}</span>
          <button id="theme" type="button" aria-label="Theme" title="Toggle light / dark" onClick={toggleTheme}>◐</button>
          <button type="button" aria-label="Keyboard shortcuts" title="Keyboard shortcuts (?)" onClick={() => setHelpOpen(true)}>?</button>
          <a id="login" hidden={!needLogin} href="./api/auth/start">Sign in with Treer</a>
        </div>
      </aside>
      <main>
        <header className="topbar">
          <h1><span id="list-title">{NAV[nav].title}</span> <span id="total" className="count">{ready ? visible.length : ""}</span></h1>
          <label className="search-label">
            <input id="search" type="search" ref={searchRef} placeholder="Search" aria-label="Search issues" value={search}
                onChange={(e) => setSearch(e.target.value)} title="Filters the list as you type; press Enter to search all titles, descriptions and comments"
                onKeyDown={(e) => { if (e.key === "Enter" && search.trim()) setDeep(search.trim()); if (e.key === "Escape") { setSearch(""); setDeep(null); e.currentTarget.blur(); } }} />
            <kbd>/</kbd>
          </label>
          <button id="refresh" type="button" className="icon-button" aria-label="Refresh" title="Refresh" disabled={!ready} onClick={reload}>↻</button>
          <button id="new" type="button" className="primary" aria-label="New issue" hidden={!ready} onClick={() => startEdit(null)}>＋ <span className="new-text">New issue</span> <kbd>C</kbd></button>
        </header>
        <section id="tracker" hidden={!ready}>
          <div className="filters">
            <label>Status
              <select id="filter-status" aria-label="Status filter" value={filters.status} onChange={(e) => { setNav("nav-all"); setFilters((f) => ({ ...f, status: e.target.value, unresolved: false })); }}>
                <option value="">All</option>{STATUSES.map((s) => <option key={s} value={s}>{s}</option>)}
              </select>
            </label>
            <div className="field-inline"><span>Assignee</span>
              <div id="assignee-filter">
                <AssigneePicker key={filters.assignee} value={filters.assignee} prompt="Anyone" ariaLabel="Assignee filter"
                    onChange={(assignee) => { setNav("nav-all"); setFilters((f) => ({ ...f, assignee, unresolved: false })); }} />
              </div>
            </div>
            <label>Label
              <input id="filter-label" placeholder="any" aria-label="Label" value={labelDraft} onChange={(e) => setLabelDraft(e.target.value)}
                  onBlur={commitLabel} onKeyDown={(e) => { if (e.key === "Enter") commitLabel(); }} />
            </label>
            <label>View
              <select id="view" aria-label="View" value={view} onChange={(e) => setView(e.target.value as View)}>
                <option value="list">List</option><option value="tree">Hierarchy tree</option>
              </select>
            </label>
            <label className="check"><input id="filter-hidden" type="checkbox" checked={filters.hidden}
                onChange={(e) => setFilters((f) => ({ ...f, hidden: e.target.checked }))} /> Hidden issues</label>
          </div>
          <div className="workspace">
            <section id="collection" aria-label="Issue list">
              <div id="list">{deep !== null ? <SearchResults q={deep} clear={() => setDeep(null)} /> : <IssueList visible={visible} view={view} collapsed={collapsed} toggle={toggleGroup} filterStatus={filters.status} route={route} focusId={focusId} />}</div>
              <button id="more" type="button" hidden={!hasMore} onClick={() => run(() => load(false))}>Load more</button>
            </section>
            <section id="detail" aria-label="Issue detail">{pane}</section>
          </div>
        </section>
        <p id="notice" role="status" aria-live="polite">
          {toast.text}
          {toast.text && toast.action && <button type="button" onClick={() => { const action = toast.action!; setToast((t) => ({ id: t.id, text: "" })); action.run(); }}>{toast.action.label}</button>}
        </p>
      </main>
      {helpOpen && <Help onClose={() => setHelpOpen(false)} />}
    </Ctx.Provider>
  );
}
