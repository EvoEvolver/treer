import { useEffect, useMemo } from "react";
import { t } from "../i18n";
import { runStatus, shortPath } from "../lib/run";
import { useStore } from "../state/store";
import { Icon } from "./Icon";

export type DrawerName = "files" | "tasks";

export function useRun() {
  const { state } = useStore();
  const thread = state.staleThread ?? state.snapshot?.thread ?? null;
  return useMemo(() => runStatus(state.snapshot, state.connected, thread), [state.snapshot, state.connected, thread]);
}

/** Background tasks exist only for harnesses that report them; the button shows how many are running. */
export function useTasks() {
  const { state } = useStore();
  const snapshot = state.snapshot;
  return { available: Boolean(snapshot?.harness.capabilities.backgroundTasks), running: (snapshot?.backgroundTasks ?? []).filter((task) => task.status === "running").length };
}

/** One compact bar at every width: worded status, what this Agent is, and three tools. The Console frames it without extra chrome. */
export function Header({ drawer, onDrawer, settingsOpen, onSettings }: { drawer: DrawerName | null; onDrawer: (name: DrawerName, trigger: HTMLElement) => void; settingsOpen: boolean; onSettings: (trigger: HTMLElement) => void }) {
  const { state } = useStore();
  const run = useRun();
  const tasks = useTasks();
  const thread = state.snapshot?.thread?.cwd ? state.snapshot.thread : state.staleThread ?? state.snapshot?.thread;
  const name = state.snapshot?.harness.name || t("app.name");
  const title = thread?.title && thread.title !== name ? thread.title : name;
  const cwd = thread?.cwd ?? "";
  useEffect(() => { document.title = run.key === "idle" ? name : `${run.label} · ${name}`; }, [run.key, run.label, name]);
  return (
    <header className="top">
      <span className={`state tone-${run.tone}`} data-state={run.key} role="status">
        {run.tone === "busy" ? <i className="spin" aria-hidden="true" /> : <i className="dot" aria-hidden="true" />}
        <span>{run.label}</span>
      </span>
      <div className="ident">
        <h1 title={title}>{title}</h1>
        {cwd && <span className="cwd" title={cwd}>{shortPath(cwd)}</span>}
      </div>
      <div className="tools">
        <button type="button" className="ib hb" aria-expanded={drawer === "files"} aria-controls="drawer-files" aria-label={t("header.files")} title={t("header.files")} onClick={(event) => onDrawer("files", event.currentTarget)}><Icon name="folder" /><span className="ib-t">{t("header.files")}</span></button>
        {tasks.available && (
          <button type="button" className="ib hb" aria-expanded={drawer === "tasks"} aria-controls="drawer-tasks" aria-label={t("header.tasksAria", { count: tasks.running })} title={t("header.tasksAria", { count: tasks.running })} onClick={(event) => onDrawer("tasks", event.currentTarget)}>
            <Icon name="tasks" /><span className="ib-t" aria-hidden="true">{t("header.tasks")}</span>{tasks.running > 0 && <span className="badge" aria-hidden="true">{tasks.running}</span>}
          </button>
        )}
        <button type="button" className="ib" aria-haspopup="dialog" aria-expanded={settingsOpen} aria-controls="settings" aria-label={t("settings.title")} title={t("settings.title")} onClick={(event) => onSettings(event.currentTarget)}><Icon name="gear" /></button>
      </div>
    </header>
  );
}
