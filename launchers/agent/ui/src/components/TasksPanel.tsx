import { useCallback, useEffect, useState } from "react";
import { fetchRaw } from "../api";
import { t, type MessageKey } from "../i18n";
import { useNow } from "../lib/clock";
import { duration } from "../lib/format";
import { useStore } from "../state/store";
import type { BackgroundTask } from "../types";
import { Icon } from "./Icon";

const ICONS: Record<BackgroundTask["kind"], string> = { shell: "term", agent: "agent", monitor: "now", other: "todo" };
const STATUSES = new Set(["running", "completed", "failed", "stopped"]);

function elapsed(task: BackgroundTask, now: number): string {
  const start = task.startedAt ? Date.parse(task.startedAt) : NaN;
  if (!Number.isFinite(start)) return t("tasks.timeUnavailable");
  const end = task.endedAt ? Date.parse(task.endedAt) : now;
  return duration(end - start);
}

function Card({ task, panelOpen }: { task: BackgroundTask; panelOpen: boolean }) {
  const [open, setOpen] = useState(false);
  const [output, setOutput] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [nonce, setNonce] = useState(0);
  const running = task.status === "running";
  const now = useNow(panelOpen && running);
  // Output tail: fetched when expanded, refreshed every 3 s while the task runs, and cancelled on collapse, close or unmount.
  useEffect(() => {
    if (!panelOpen || !open) return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const run = async () => {
      try {
        const response = await fetchRaw(`api/tasks/${encodeURIComponent(task.id)}/output?tail=65536`, { signal: controller.signal });
        if (!response.ok) throw new Error(t("tasks.outputUnavailable", { status: response.status }));
        // Older task servers use a JSON envelope; the contract also permits plain text.
        const content = response.headers.get("content-type")?.includes("application/json") ? ((await response.json()) as { content?: string }).content ?? "" : await response.text();
        if (controller.signal.aborted) return;
        setOutput(content); setError(null);
      } catch (failure) {
        if (!controller.signal.aborted) setError((failure as Error).message);
      }
      if (!controller.signal.aborted && running) timer = setTimeout(() => void run(), 3000);
    };
    void run();
    return () => { controller.abort(); clearTimeout(timer); };
  }, [panelOpen, open, task.id, task.status, running, nonce]);
  const status = STATUSES.has(task.status) ? task.status : "unknown";
  return (
    <details className={`task task-${status}`} open={open} onToggle={(event) => setOpen(event.currentTarget.open)}>
      <summary>
        <Icon name={ICONS[task.kind] ?? ICONS.other} />
        <span className="td">{task.description || task.id}</span>
        <span className={`ts${running ? " run" : ""}`}>{t(`tasks.status.${status}` as MessageKey)}</span>
        <span className="tm">{task.kind || "other"} · {elapsed(task, now)}{task.exitCode == null ? "" : ` · ${t("tasks.exit", { code: task.exitCode })}`}</span>
        {task.summary && <span className="task-summary">{task.summary}</span>}
      </summary>
      <pre className="task-output" tabIndex={0}>{output === null && !error ? t("tasks.loadingOutput") : output || t("tasks.noOutput")}</pre>
      {error && <p className="field-error" role="status">{error}</p>}
      <button type="button" className="btn" onClick={() => setNonce((value) => value + 1)}>{t("tasks.refresh")}</button>
    </details>
  );
}

/** "Clear finished tasks": removes only the launcher's finished records after a confirmation; running tasks and output files stay. */
function ClearFinished({ count }: { count: number }) {
  const { actions } = useStore();
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => { if (count === 0) setConfirming(false); }, [count]);
  if (count === 0) return null;
  const clear = async () => {
    setBusy(true);
    const failure = await actions.clearFinishedTasks();
    setBusy(false);
    if (failure) setError(t("tasks.clearFailed", { message: failure })); else { setError(null); setConfirming(false); }
  };
  return (
    <footer className="tasks-foot">
      {confirming ? (
        <div className="goal-confirm" role="group" aria-label={t("tasks.clearFinished")}>
          <span>{t("tasks.clearQuestion", { count })}</span>
          <button type="button" className="btn danger tasks-btn" disabled={busy} onClick={() => void clear()} autoFocus>{t("tasks.clearConfirm")}</button>
          <button type="button" className="btn tasks-btn" onClick={() => { setConfirming(false); setError(null); }}>{t("tasks.clearKeep")}</button>
        </div>
      ) : <button type="button" className="btn tasks-btn" onClick={() => setConfirming(true)}>{t("tasks.clearFinished")}</button>}
      {error && <p className="field-error" role="status">{error}</p>}
    </footer>
  );
}

export function TasksPanel({ open, onClose }: { open: boolean; onClose: () => void }) {
  const { state } = useStore();
  const tasks = state.snapshot?.backgroundTasks ?? [];
  const finished = tasks.filter((task) => task.status !== "running").length;
  const close = useCallback(onClose, [onClose]);
  return (
    <aside className="panel tasks" id="tasks" aria-label={t("tasks.title")} hidden={!open} onKeyDown={(event) => { if (event.key === "Escape" && !document.querySelector("dialog[open]")) { event.preventDefault(); close(); } }}>
      <header className="ph"><h2>{t("tasks.title")}</h2><button type="button" className="xbtn" aria-label={t("tasks.close")} onClick={close}><Icon name="close" /></button></header>
      <div className="tlist">
        {tasks.length === 0 && <p className="tasks-empty">{t("tasks.empty")}</p>}
        {tasks.map((task) => <Card key={task.id} task={task} panelOpen={open} />)}
      </div>
      <ClearFinished count={finished} />
    </aside>
  );
}
