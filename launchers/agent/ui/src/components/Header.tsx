import { forwardRef } from "react";
import { t } from "../i18n";
import { toggleTheme } from "../appearance";
import { useStore } from "../state/store";
import type { MessageKey } from "../i18n";
import { Icon } from "./Icon";

export type PanelName = "files" | "tasks";
export interface PanelProps { panels: Record<PanelName, boolean>; onPanel: (name: PanelName, trigger: HTMLElement) => void }

/** The look button. Rendered in the header and, for the rail look, in the rail; CSS shows the one that fits. */
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
      </div>
    </header>
  );
}

export function Rail({ menuOpen, onToggleMenu, panels, onPanel }: { menuOpen: boolean; onToggleMenu: () => void } & PanelProps) {
  const tasks = useTasks();
  return (
    <nav className="rail" aria-label={t("tabbar.label")}>
      <button type="button" className="rbtn" aria-expanded={panels.files} aria-controls="files" aria-label={t("header.filesAria")} title={t("header.files")} onClick={(event) => onPanel("files", event.currentTarget)}><Icon name="folder" /></button>
      {tasks.available && <button type="button" className="rbtn" aria-expanded={panels.tasks} aria-controls="tasks" aria-label={t("header.tasksAria", { count: tasks.running })} title={t("tabbar.tasks")} onClick={(event) => onPanel("tasks", event.currentTarget)}><Icon name="tasks" />{tasks.running > 0 && <span className="badge">{tasks.running}</span>}</button>}
      <span className="rail-sp" />
      <LookButton className="rbtn" open={menuOpen} onToggle={onToggleMenu} />
      <button type="button" className="rbtn" aria-label={t("appearance.toggleTheme")} title={t("appearance.toggleTheme")} onClick={toggleTheme}><Icon name="theme" /></button>
    </nav>
  );
}

export function Tabbar({ menuOpen, onToggleMenu, panels, onPanel }: { menuOpen: boolean; onToggleMenu: () => void } & PanelProps) {
  const tasks = useTasks();
  return (
    <nav className="tabbar" aria-label={t("tabbar.label")}>
      <button type="button" aria-expanded={panels.files} aria-controls="files" onClick={(event) => onPanel("files", event.currentTarget)}><Icon name="folder" /><span>{t("tabbar.files")}</span></button>
      {tasks.available && <button type="button" aria-expanded={panels.tasks} aria-controls="tasks" onClick={(event) => onPanel("tasks", event.currentTarget)}><Icon name="tasks" /><span>{t("tabbar.tasks")}</span></button>}
      <button type="button" aria-haspopup="dialog" aria-expanded={menuOpen} aria-controls="appearance" onClick={onToggleMenu}><Icon name="look" /><span>{t("tabbar.look")}</span></button>
      <button type="button" onClick={toggleTheme}><Icon name="theme" /><span>{t("tabbar.theme")}</span></button>
    </nav>
  );
}
