import { Fragment, useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { useApp } from "../context";
import { displayName, principalContext } from "../lib";
import type { Principal } from "../types";
import { Avatar } from "./Avatar";

interface Branch { folders: Map<string, Branch>; agents: Principal[] }

// Agents are grouped by the folders in their Tree path; malformed or missing paths stay selectable by stable ID.
function agentTree(agents: Principal[]) {
  const root: Branch = { folders: new Map(), agents: [] };
  const unplaced: Principal[] = [];
  for (const agent of agents) {
    if (typeof agent.path !== "string" || !agent.path.startsWith("/") || agent.path === "/" || agent.path.includes("//")) {
      unplaced.push(agent);
      continue;
    }
    const parts = agent.path.slice(1).split("/");
    parts.pop();
    let branch = root;
    for (const part of parts) {
      if (!branch.folders.has(part)) branch.folders.set(part, { folders: new Map(), agents: [] });
      branch = branch.folders.get(part)!;
    }
    branch.agents.push(agent);
  }
  return { root, unplaced };
}

export function AssigneePicker({ value, onChange, prompt = "Unassigned", ariaLabel = "Assignee", disabled }: {
  value: string; onChange: (id: string) => void; prompt?: string; ariaLabel?: string; disabled?: boolean;
}) {
  const { people } = useApp();
  const [open, setOpen] = useState(false), [query, setQuery] = useState("");
  const wrapper = useRef<HTMLDivElement>(null), trigger = useRef<HTMLButtonElement>(null);
  const popup = useRef<HTMLDivElement>(null), search = useRef<HTMLInputElement>(null);
  const person = people.find((p) => p.id === value);
  const close = (focus = false) => { setOpen(false); if (focus) trigger.current?.focus(); };
  const choose = (id: string) => { close(true); onChange(id); };

  useEffect(() => {
    if (!open) return;
    const away = (event: PointerEvent) => { if (!wrapper.current?.contains(event.target as Node)) setOpen(false); };
    document.addEventListener("pointerdown", away);
    search.current?.focus();
    return () => document.removeEventListener("pointerdown", away);
  }, [open]);
  // Keep the popup inside the viewport on narrow screens.
  useLayoutEffect(() => {
    const el = popup.current;
    if (!open || !el) return;
    el.style.left = "0px";
    const bounds = el.getBoundingClientRect();
    el.style.left = Math.max(12, Math.min(bounds.left, innerWidth - bounds.width - 12)) - bounds.left + "px";
  }, [open]);

  const needle = query.trim().toLowerCase();
  const visible = people.filter((p) => !needle || `${p.name} ${p.path ?? ""} ${p.id}`.toLowerCase().includes(needle));
  const humans = visible.filter((p) => p.kind === "human");
  const agents = visible.filter((p) => p.kind === "agent");
  const { root, unplaced } = useMemo(() => agentTree(agents), [people, query]);

  const option = (p: Principal | null): ReactNode => {
    const id = p?.id ?? "";
    return (
      <button type="button" key={id || "none"} className="picker-option" data-principal-id={id} aria-pressed={id === value}
          aria-label={p ? `${p.name} · ${p.kind} · ${principalContext(p)}` : prompt} onClick={() => choose(id)}>
        <Avatar person={p} />
        <span className="person-label">{p?.name ?? prompt}{p && <small className="person-path">{principalContext(p)}</small>}</span>
        {id === value && <span>✓</span>}
      </button>
    );
  };
  const branch = (node: Branch, path: string): ReactNode[] => [
    ...[...node.folders].sort((a, b) => a[0].localeCompare(b[0])).map(([folder, sub]) => (
      <details key={path + "/" + folder} className="picker-folder" open>
        <summary title={path + "/" + folder}>▱ {folder}</summary>
        <div className="picker-folder-group">{branch(sub, path + "/" + folder)}</div>
      </details>
    )),
    ...[...node.agents].sort((a, b) => a.name.localeCompare(b.name)).map(option),
  ];

  const keydown = (event: KeyboardEvent) => {
    if (event.key === "Escape") { event.preventDefault(); close(true); return; }
    const items = [...popup.current!.querySelectorAll<HTMLElement>("input,summary,button")].filter((e) => e.getClientRects().length);
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const index = items.indexOf(document.activeElement as HTMLElement), step = event.key === "ArrowDown" ? 1 : -1;
      items[(index + step + items.length) % items.length]?.focus();
    }
    if (event.key === "Tab" && (event.shiftKey ? document.activeElement === items[0] : document.activeElement === items.at(-1))) {
      event.preventDefault();
      close(true);
    }
  };

  return (
    <div className="assignee-picker" ref={wrapper}>
      <button type="button" className="assignee-trigger" ref={trigger} aria-label={ariaLabel} aria-haspopup="dialog" aria-expanded={open}
          disabled={disabled} title={person ? displayName(people, value) : value || prompt} onClick={() => { setQuery(""); setOpen(!open); }}>
        <Avatar person={person} />
        <span className="selected-name">{person?.name ?? (value ? displayName(people, value) + " (unavailable)" : prompt)}
          {person && <small className="person-path">{principalContext(person)}</small>}</span>
        <span className="chevron">⌄</span>
      </button>
      {open && (
        <div className="picker-popup" ref={popup} role="dialog" aria-label="Choose assignee" onKeyDown={keydown}>
          <input type="search" className="picker-search" ref={search} placeholder="Search people or Agent paths…" aria-label="Search assignees"
              value={query} onChange={(e) => setQuery(e.target.value)} />
          <div className="picker-content">
            {option(null)}
            {humans.length > 0 && <div className="picker-heading">PEOPLE</div>}
            {humans.map(option)}
            {agents.length > 0 && <div className="picker-heading">AGENT TREE</div>}
            {branch(root, "")}
            {unplaced.length > 0 && (
              <Fragment>
                <div className="picker-heading">PLACEMENT UNAVAILABLE</div>
                <p className="picker-empty">Folder placement unavailable. Agent IDs distinguish these entries.</p>
                {unplaced.map(option)}
              </Fragment>
            )}
            {!visible.length && <p className="picker-empty">No matching people or Agents</p>}
          </div>
        </div>
      )}
    </div>
  );
}
