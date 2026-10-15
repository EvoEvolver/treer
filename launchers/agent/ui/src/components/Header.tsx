import { forwardRef, useEffect, useRef, useState, type KeyboardEvent } from "react";
import { t } from "../i18n";
import { toggleTheme } from "../appearance";
import { useStore } from "../state/store";
import type { MessageKey } from "../i18n";
import { Icon } from "./Icon";

export type PanelName = "files" | "tasks";
export interface PanelProps { panels: Record<PanelName, boolean>; onPanel: (name: PanelName, trigger: HTMLElement) => void }

/** The look button, in the header actions of every look (phones use the bottom bar instead). */
export const LookButton = forwardRef<HTMLButtonElement, { className: string; open: boolean; onToggle: () => void }>(function LookButton({ className, open, onToggle }, ref) {
  return (
    <button ref={ref} type="button" className={className} aria-haspopup="dialog" aria-expanded={open} aria-controls="appearance" aria-label={t("appearance.open")} title={t("appearance.open")} onClick={onToggle}>
      <Icon name="look" />
    </button>
  );
});

export function useStatus(): { state: "starting" | "working" | "blocked" | "idle" | "reconnecting"; label: string } {
  const { state } = useStore();
  const runtime = state.snapshot?.runtime.state ?? "starting";
  const key: "starting" | "working" | "blocked" | "idle" | "reconnecting" = state.connected ? runtime : "reconnecting";
  return { state: key, label: t(`status.${key}` as MessageKey) };
}

/** Background tasks exist only for harnesses that report them; the button shows how many are running. */
export function useTasks() {
  const { state } = useStore();
  const snapshot = state.snapshot;
  return { available: Boolean(snapshot?.harness.capabilities.backgroundTasks), running: (snapshot?.backgroundTasks ?? []).filter((task) => task.status === "running").length };
}

export function Header({ menuOpen, onToggleMenu, lookRef, panels, onPanel }: { menuOpen: boolean; onToggleMenu: () => void; lookRef: React.Ref<HTMLButtonElement> } & PanelProps) {
  const { state } = useStore();
  const status = useStatus();
  const tasks = useTasks();
  const name = state.snapshot?.harness.name || t("app.name");
  const title = state.snapshot?.thread?.title || name;
  return (
    <header className="top">
      <div className="brand">
        <span className="mark" aria-hidden="true"><span className="m-a">●</span><span className="m-b">{name.charAt(0).toUpperCase()}</span><span className="m-c">▌</span></span>
        <div className="titles">
          <h1>{title}</h1>
          <p className="cwd">{state.snapshot?.thread?.cwd || t("header.cwdConnecting")}</p>
        </div>
      </div>
      <div className="actions">
        <span className={`status status-${status.state}`} role="status"><i className="dot" /><span className="status-t">{status.label}</span></span>
        {tasks.available && <button type="button" className="tbtn" aria-expanded={panels.tasks} aria-controls="tasks" aria-label={t("header.tasksAria", { count: tasks.running })} onClick={(event) => onPanel("tasks", event.currentTarget)}><Icon name="tasks" /><span className="lbl">{t("header.tasks", { count: tasks.running })}</span></button>}
        <button type="button" className="tbtn" aria-expanded={panels.files} aria-controls="files" aria-label={t("header.filesAria")} onClick={(event) => onPanel("files", event.currentTarget)}><Icon name="folder" /><span className="lbl">{t("header.files")}</span></button>
        <LookButton ref={lookRef} className="tbtn" open={menuOpen} onToggle={onToggleMenu} />
        <button type="button" className="tbtn" aria-label={t("appearance.toggleTheme")} title={t("appearance.toggleTheme")} onClick={toggleTheme}><Icon name="theme" /></button>
        <NavMenu panels={panels} onPanel={onPanel} onAppearance={onToggleMenu} />
      </div>
    </header>
  );
}

/** Phone-only menu in the header (the bottom bar is gone): Files, Tasks, Appearance and Theme in one keyboard-reachable menu. */
export function NavMenu({ panels: _panels, onPanel, onAppearance }: PanelProps & { onAppearance: () => void }) {
  const tasks = useTasks();
  const [open, setOpen] = useState(false);
  const button = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const close = (returnFocus: boolean) => { setOpen(false); if (returnFocus) button.current?.focus(); };
  useEffect(() => {
    if (!open) return;
    list.current?.querySelector<HTMLElement>("[role=menuitem]")?.focus();
    const onPointer = (event: PointerEvent) => { const target = event.target as Element | null; if (!list.current?.contains(target) && !button.current?.contains(target)) close(false); };
    document.addEventListener("pointerdown", onPointer);
    return () => document.removeEventListener("pointerdown", onPointer);
  }, [open]);
  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    const items = [...(list.current?.querySelectorAll<HTMLElement>("[role=menuitem]") ?? [])];
    const index = items.indexOf(document.activeElement as HTMLElement);
    const go = (i: number) => { event.preventDefault(); items[(i + items.length) % items.length]?.focus(); };
    if (event.key === "Escape") { event.preventDefault(); close(true); }
    else if (event.key === "ArrowDown") go(index + 1);
    else if (event.key === "ArrowUp") go(index - 1);
    else if (event.key === "Home") go(0);
    else if (event.key === "End") go(items.length - 1);
    else if (event.key === "Tab") close(false);
  }
  const choose = (action: () => void) => () => { close(false); action(); };
  return (
    <>
      <button ref={button} type="button" className="nav-btn" aria-haspopup="menu" aria-expanded={open} aria-controls="navmenu" aria-label={t("nav.menu")} title={t("nav.menu")} onClick={() => setOpen((value) => !value)}><Icon name="menu" /></button>
      {open && (
        <div className="navmenu" id="navmenu" role="menu" aria-label={t("nav.menu")} ref={list} onKeyDown={onKeyDown}>
          <button type="button" role="menuitem" className="navitem" onClick={choose(() => button.current && onPanel("files", button.current))}><Icon name="folder" /><span>{t("nav.files")}</span></button>
          {tasks.available && <button type="button" role="menuitem" className="navitem" onClick={choose(() => button.current && onPanel("tasks", button.current))}><Icon name="tasks" /><span>{t("nav.tasks")}</span>{tasks.running > 0 && <span className="navcount">{tasks.running}</span>}</button>}
          <button type="button" role="menuitem" className="navitem" onClick={choose(onAppearance)}><Icon name="look" /><span>{t("nav.appearance")}</span></button>
          <button type="button" role="menuitem" className="navitem" onClick={choose(toggleTheme)}><Icon name="theme" /><span>{t("nav.theme")}</span></button>
        </div>
      )}
    </>
  );
}
