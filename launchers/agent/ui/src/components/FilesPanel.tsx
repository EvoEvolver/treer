import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties, type KeyboardEvent } from "react";
import { fileDownloadUrl, request } from "../api";
import { t } from "../i18n";
import { safeUrl } from "../lib/url";
import type { FileEntry, FilePreview } from "../types";
import { useDialogs } from "./Dialogs";
import { Icon } from "./Icon";
import { FileContent } from "./FileContent";
import { previewKind } from "./previews/kind";
import { insertIntoDraft } from "../state/draft";

// The server refuses larger files with 413 before sending a byte; the UI says so up front instead of starting a failed download.
const DOWNLOAD_LIMIT_BYTES = 1024 ** 3;

type Preview = { state: "idle" } | { state: "loading" } | { state: "ready"; file: FilePreview } | { state: "error"; message: string };
type Row = { kind: "entry"; entry: FileEntry; depth: number; open: boolean } | { kind: "notice"; key: string; depth: number; text: string; retry?: () => void };

/** Read-only workspace browser: lazy folders, keyboard tree, client-side filter over what is loaded, immediate preview. */
export function FilesPanel({ open, requestedFile, onClose }: { open: boolean; requestedFile?: { path: string; revision: number } | null; onClose: () => void }) {
  const [cache, setCache] = useState<ReadonlyMap<string, FileEntry[]>>(new Map());
  const [errors, setErrors] = useState<ReadonlyMap<string, string>>(new Map());
  const [pending, setPending] = useState<ReadonlySet<string>>(new Set());
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set());
  const [selected, setSelected] = useState<string | null>(null);
  const [preview, setPreview] = useState<Preview>({ state: "idle" });
  const [filter, setFilter] = useState("");
  const [tab, setTab] = useState<"browse" | "preview">("browse");
  const [focusPath, setFocusPath] = useState<string | null>(null);
  const cacheRef = useRef(cache);
  const pendingRef = useRef(pending);
  const version = useRef(0);
  const previewRequest = useRef<AbortController | null>(null);
  const list = useRef<HTMLUListElement>(null);
  const closeButton = useRef<HTMLButtonElement>(null);
  const { openText } = useDialogs();
  cacheRef.current = cache;
  pendingRef.current = pending;

  const load = useCallback(async (path: string, refresh = false) => {
    if (pendingRef.current.has(path) || (!refresh && cacheRef.current.has(path))) return;
    pendingRef.current = new Set(pendingRef.current).add(path);
    setPending(pendingRef.current);
    setErrors((current) => { const next = new Map(current); next.delete(path); return next; });
    try {
      const response = await request<{ entries: FileEntry[] }>(`v1/files/tree?path=${encodeURIComponent(path)}`);
      setCache((current) => new Map(current).set(path, response.entries));
    } catch (error) {
      setErrors((current) => new Map(current).set(path, (error as Error).message));
    } finally {
      const next = new Set(pendingRef.current); next.delete(path); pendingRef.current = next; setPending(next);
    }
  }, []);
  // Opening the panel starts on the tree (on a phone the preview is a separate tab); the folder cache and selection are kept.
  useEffect(() => {
    if (open) { void load(""); setTab("browse"); }
    else { ++version.current; previewRequest.current?.abort(); setPreview({ state: "idle" }); }
    return () => { ++version.current; previewRequest.current?.abort(); };
  }, [open, load]);

  const select = useCallback(async (path: string) => {
    setSelected(path);
    setTab("preview");
    const mine = ++version.current;
    previewRequest.current?.abort();
    const controller = new AbortController();
    previewRequest.current = controller;
    setPreview({ state: "loading" });
    try {
      const file = await request<FilePreview>(`v1/files?path=${encodeURIComponent(path)}`, { signal: controller.signal });
      if (mine === version.current) setPreview({ state: "ready", file });
    } catch (error) {
      if (mine === version.current) setPreview({ state: "error", message: (error as Error).message });
    }
  }, []);
  useEffect(() => {
    if (!open || !requestedFile) return;
    closeButton.current?.focus();
    const path = requestedFile.path;
    setFilter(""); setFocusPath(path);
    const parts = path.split("/");
    if (!path.startsWith("/") && !parts.includes("..") && !path.includes(":")) {
      const parents = parts.slice(0, -1).map((_, index) => parts.slice(0, index + 1).join("/"));
      setExpanded(current => new Set([...current, ...parents]));
      for (const parent of parents) void load(parent);
    }
    void select(path);
  }, [open, requestedFile, load, select]);
  function toggleDirectory(path: string) {
    setExpanded((current) => {
      const next = new Set(current);
      if (next.has(path)) next.delete(path); else { next.add(path); void load(path); }
      return next;
    });
  }
  function refresh() {
    void load("", true);
    for (const path of expanded) void load(path, true);
    if (selected) void select(selected);
  }

  const query = filter.trim().toLowerCase();
  const rows = useMemo<Row[]>(() => {
    const out: Row[] = [];
    const deepMatch = (directory: string): boolean => (cache.get(directory) ?? []).some((entry) => entry.name.toLowerCase().includes(query) || (entry.kind === "directory" && deepMatch(entry.path)));
    const walk = (directory: string, depth: number) => {
      for (const entry of cache.get(directory) ?? []) {
        const isDirectory = entry.kind === "directory";
        const named = !query || entry.name.toLowerCase().includes(query);
        const below = isDirectory && query ? deepMatch(entry.path) : false;
        if (query && !named && !below) continue;
        const isOpen = isDirectory && (expanded.has(entry.path) || below);
        out.push({ kind: "entry", entry, depth, open: isOpen });
        if (isDirectory && isOpen) walk(entry.path, depth + 1);
      }
      if (pending.has(directory)) out.push({ kind: "notice", key: `p:${directory}`, depth, text: t("files.loading") });
      else if (errors.has(directory)) out.push({ kind: "notice", key: `e:${directory}`, depth, text: errors.get(directory) ?? "", retry: () => void load(directory, true) });
      else if (cache.has(directory) && (cache.get(directory)?.length ?? 0) === 0 && !query) out.push({ kind: "notice", key: `m:${directory}`, depth, text: t("files.empty") });
    };
    walk("", 0);
    if (query && !out.some((row) => row.kind === "entry")) out.push({ kind: "notice", key: "none", depth: 0, text: t("files.noMatch", { query: filter.trim() }) });
    return out;
  }, [cache, errors, pending, expanded, query, filter, load]);

  const entryRows = rows.filter((row): row is Extract<Row, { kind: "entry" }> => row.kind === "entry");
  const tabbable = entryRows.some((row) => row.entry.path === focusPath) ? focusPath : (entryRows.find((row) => row.entry.path === selected) ?? entryRows[0])?.entry.path;

  function onKeyDown(event: KeyboardEvent<HTMLUListElement>) {
    const active = document.activeElement as HTMLElement | null;
    if (active?.dataset.dl !== undefined) {
      // A row's download link is reached with ArrowRight and left with ArrowLeft; Enter downloads natively.
      if (event.key === "ArrowLeft") { event.preventDefault(); list.current?.querySelector<HTMLElement>(`[role=treeitem][data-path="${CSS.escape(active.dataset.dl)}"]`)?.focus(); }
      return;
    }
    const items = [...(list.current?.querySelectorAll<HTMLElement>("[role=treeitem]") ?? [])];
    const index = items.indexOf(document.activeElement as HTMLElement);
    if (index < 0) return;
    const go = (target: number) => { const element = items[Math.max(0, Math.min(items.length - 1, target))]; if (element) { setFocusPath(element.dataset.path ?? null); element.focus(); } };
    const row = entryRows[index];
    if (event.key === "ArrowDown") { event.preventDefault(); go(index + 1); }
    else if (event.key === "ArrowUp") { event.preventDefault(); go(index - 1); }
    else if (event.key === "Home") { event.preventDefault(); go(0); }
    else if (event.key === "End") { event.preventDefault(); go(items.length - 1); }
    else if (event.key === "ArrowRight" && row?.entry.kind === "file") {
      const link = items[index]?.parentElement?.querySelector<HTMLElement>("[data-dl]");
      if (link) { event.preventDefault(); link.focus(); }
    } else if (event.key === "ArrowRight" && row?.entry.kind === "directory") {
      event.preventDefault();
      if (!row.open) toggleDirectory(row.entry.path); else go(index + 1);
    } else if (event.key === "ArrowLeft" && row) {
      event.preventDefault();
      if (row.entry.kind === "directory" && row.open && !query) toggleDirectory(row.entry.path);
      else for (let i = index - 1; i >= 0; i--) if ((entryRows[i]?.depth ?? 0) < row.depth) { go(i); break; }
    }
  }

  const file = preview.state === "ready" ? preview.file : null;
  const meta = preview.state === "loading" ? t("files.loading") : preview.state === "error" ? t("files.unavailable") : file ? `${t("files.bytes", { n: file.size.toLocaleString() })} · ${file.truncated ? t("files.truncated") : t("files.readOnly")}` : t("files.readOnly");
  return (
    <aside className="drawer files" id="drawer-files" aria-label={t("files.title")} hidden={!open} data-ftab={tab} onKeyDown={(event) => { if (event.key === "Escape" && !document.querySelector("dialog[open]")) { event.preventDefault(); onClose(); } }}>
      <header className="drawer-h"><h2>{t("files.title")}</h2><button type="button" className="ib" aria-label={t("files.refreshTitle")} title={t("files.refreshTitle")} onClick={refresh}><Icon name="retry" /></button><button type="button" className="ib" ref={closeButton} aria-label={t("files.close")} onClick={onClose}><Icon name="close" /></button></header>
      <label className="filter"><Icon name="search" /><span className="sr-only">{t("files.filter")}</span><input type="search" value={filter} placeholder={t("files.filter")} autoComplete="off" onChange={(event) => setFilter(event.target.value)} /></label>
      <ul className="tree" role="tree" aria-label={t("files.tree")} ref={list} onKeyDown={onKeyDown}>
        {rows.map((row) => row.kind === "notice" ? (
          <li key={row.key} role="none" className="tree-empty">{row.text}{row.retry && <> <button type="button" className="btn" onClick={row.retry}>{t("files.retry")}</button></>}</li>
        ) : (
          <li key={row.entry.path} role="none" className={row.entry.kind === "file" ? "has-dl" : undefined}>
            <button type="button" role="treeitem" className={`trow${selected === row.entry.path ? " sel" : ""}`} data-path={row.entry.path} title={row.entry.path} aria-level={row.depth + 1}
              aria-expanded={row.entry.kind === "directory" ? row.open : undefined} aria-selected={row.entry.kind !== "directory" ? selected === row.entry.path : undefined} tabIndex={tabbable === row.entry.path ? 0 : -1}
              style={{ "--lvl": row.depth } as CSSProperties} onFocus={() => setFocusPath(row.entry.path)} onClick={() => (row.entry.kind === "directory" ? toggleDirectory(row.entry.path) : void select(row.entry.path))}>
              {row.entry.kind === "directory" ? <Icon name="chev" className="chev" /> : <span className="chev-sp" />}
              <Icon name={row.entry.kind === "directory" ? "folder" : "file"} /><span className="tn">{row.entry.name}</span>
            </button>
            {row.entry.kind === "file" && ((row.entry.size ?? 0) > DOWNLOAD_LIMIT_BYTES
              ? <button type="button" className="trow-dl" disabled title={t("files.downloadTooLarge")} aria-label={`${t("files.downloadFile", { name: row.entry.name })}: ${t("files.downloadTooLarge")}`}><Icon name="download" /></button>
              : <a className="trow-dl" href={safeUrl(fileDownloadUrl(row.entry.path))} download={row.entry.name} data-dl={row.entry.path} tabIndex={-1} title={t("files.downloadFile", { name: row.entry.name })} aria-label={t("files.downloadFile", { name: row.entry.name })}><Icon name="download" /></a>)}
          </li>
        ))}
      </ul>
      <section className={`preview${file && previewKind(file.path) !== "text" ? " rich-preview" : ""}`} aria-label={t("files.preview")}>
        <div className="pmeta">
          <button type="button" className="btn small back" onClick={() => { setTab("browse"); requestAnimationFrame(() => list.current?.querySelector<HTMLElement>("[tabindex='0']")?.focus()); }}><Icon name="back" />{t("files.back")}</button>
          <b title={selected ?? undefined}>{selected ?? t("files.select")}</b><span className={file?.truncated ? "truncated" : undefined}>{meta}</span>
          {selected && preview.state !== "idle" && <span className="pactions">
            <button type="button" className="btn small" title={t("files.mentionTitle")} onClick={() => { insertIntoDraft(selected); if (matchMedia("(max-width: 1099px)").matches) onClose(); }}><Icon name="at" />{t("files.mention")}</button>
            {file && ["text", "markdown"].includes(previewKind(file.path)) && <button type="button" className="btn small" onClick={() => openText(file.path, file.content)}><Icon name="open" />{t("files.open")}</button>}
            {file && file.size > DOWNLOAD_LIMIT_BYTES
              ? <><button type="button" className="btn small" disabled title={t("files.downloadTooLarge")}><Icon name="download" />{t("files.download")}</button><span className="dl-note" role="status">{t("files.downloadTooLarge")}</span></>
              : <a className="btn small" href={safeUrl(fileDownloadUrl(selected))} download={selected.split("/").pop()} title={t("files.downloadFile", { name: selected })}><Icon name="download" />{t("files.download")}</a>}
          </span>}
        </div>
        {preview.state === "error" && <p className="field-error" role="status">{preview.message}</p>}
        {open && file ? <FileContent key={`${file.path}:${version.current}`} file={file} /> : <p className="muted phint">{preview.state === "idle" ? t("files.hint") : ""}</p>}
      </section>
    </aside>
  );
}
