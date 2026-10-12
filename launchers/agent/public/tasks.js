const statusLabels = { running: "Running", completed: "Completed", failed: "Failed", stopped: "Stopped" };
const icons = { shell: ">_", agent: "◇", monitor: "◉", other: "•" };

export function duration(start, end = null) {
  if (!start || !Number.isFinite(Date.parse(start))) return "Time unavailable";
  const seconds = Math.max(0, Math.floor(((end ? Date.parse(end) : Date.now()) - Date.parse(start)) / 1000));
  return seconds >= 3600 ? `${Math.floor(seconds / 3600)}h ${Math.floor(seconds % 3600 / 60)}m` : seconds >= 60 ? `${Math.floor(seconds / 60)}m ${seconds % 60}s` : `${seconds}s`;
}

function textNode(tag, className, text) {
  const node = document.createElement(tag);
  node.className = className;
  if (text != null) node.textContent = text;
  return node;
}

export function setupTasks() {
  const panel = document.querySelector("#tasks-panel");
  const list = document.querySelector("#tasks-list");
  const toggle = document.querySelector("#tasks-toggle");
  const cards = new Map();
  let available = false;
  let elapsedTimer;

  function cancel(card) {
    clearTimeout(card.timer);
    card.controller?.abort();
    card.controller = null;
  }
  async function output(card) {
    if (panel.hidden || !card.details.open || card.controller) return;
    const controller = new AbortController();
    card.controller = controller;
    try {
      const response = await fetch(`api/tasks/${encodeURIComponent(card.task.id)}/output?tail=65536`, { signal: controller.signal });
      if (!response.ok) throw new Error(`Output unavailable (HTTP ${response.status})`);
      // Older task servers use a JSON envelope; the new contract also permits plain text.
      const content = response.headers.get("content-type")?.includes("application/json") ? (await response.json()).content : await response.text();
      if (controller.signal.aborted) return;
      card.output.textContent = content || "No output yet.";
      card.error.hidden = true;
    } catch (error) {
      if (!controller.signal.aborted) {
        card.error.textContent = error.message;
        card.error.hidden = false;
      }
    } finally {
      if (card.controller === controller) card.controller = null;
      if (!controller.signal.aborted && !panel.hidden && card.details.open && card.task.status === "running") {
        card.timer = setTimeout(() => void output(card), 3000);
      }
    }
  }
  function makeCard(task) {
    const details = textNode("details", "task-card");
    const summary = textNode("summary", "task-heading");
    const icon = textNode("span", "task-icon");
    icon.setAttribute("aria-hidden", "true");
    const description = textNode("strong", "task-description");
    const status = textNode("span", "task-status");
    summary.append(icon, description, status);
    const meta = textNode("p", "task-meta");
    const summaryText = textNode("p", "task-summary");
    const body = textNode("div", "task-body");
    const outputNode = textNode("pre", "task-output", "Loading output…");
    outputNode.tabIndex = 0;
    const error = textNode("p", "task-error");
    error.setAttribute("role", "status");
    error.hidden = true;
    const refresh = textNode("button", "task-refresh", "Refresh output");
    refresh.type = "button";
    const card = { task, details, icon, description, status, meta, summaryText, output: outputNode, error, controller: null, timer: null };
    refresh.addEventListener("click", () => { cancel(card); void output(card); });
    details.addEventListener("toggle", () => {
      cancel(card);
      if (details.open) void output(card);
    });
    body.append(outputNode, error, refresh);
    summary.append(meta, summaryText);
    details.append(summary, body);
    return card;
  }
  function updateMeta(card) {
    const task = card.task;
    card.meta.textContent = `${task.kind || "other"} · ${duration(task.startedAt, task.endedAt)}${task.exitCode == null ? "" : ` · Exit ${task.exitCode}`}`;
  }
  function open(show, focus = true) {
    panel.hidden = !show;
    toggle.setAttribute("aria-expanded", String(show));
    clearInterval(elapsedTimer);
    for (const card of cards.values()) {
      cancel(card);
      if (show && card.details.open) void output(card);
    }
    if (show) {
      document.dispatchEvent(new CustomEvent("agent-ui:panel", { detail: "tasks" }));
      elapsedTimer = setInterval(() => { for (const card of cards.values()) if (card.task.status === "running") updateMeta(card); }, 1000);
    }
    if (focus) (show ? document.querySelector("#tasks-close") : toggle).focus();
  }
  toggle.addEventListener("click", () => open(panel.hidden));
  document.querySelector("#tasks-close").addEventListener("click", () => open(false));
  panel.addEventListener("keydown", (event) => { if (event.key === "Escape") { event.preventDefault(); open(false); } });
  document.addEventListener("agent-ui:panel", (event) => { if (event.detail === "files" && matchMedia("(max-width: 720px)").matches) open(false, false); });
  matchMedia("(max-width: 720px)").addEventListener("change", (event) => {
    if (event.matches && !document.querySelector("#file-browser").hidden) open(false, false);
  });

  return function render(snapshot) {
    available = Boolean(snapshot?.harness?.capabilities?.backgroundTasks);
    toggle.hidden = !available;
    if (!available) { open(false, false); return; }
    const tasks = snapshot.backgroundTasks ?? [];
    const running = tasks.filter((task) => task.status === "running").length;
    toggle.textContent = `${running} running`;
    toggle.classList.toggle("tasks-idle", running === 0);
    toggle.setAttribute("aria-label", `Background tasks, ${running} running`);
    for (const [id, card] of cards) if (!tasks.some((task) => task.id === id)) { cancel(card); card.details.remove(); cards.delete(id); }
    list.querySelector(".tasks-empty")?.remove();
    for (const task of tasks) {
      let card = cards.get(task.id);
      if (!card) { card = makeCard(task); cards.set(task.id, card); list.append(card.details); }
      const changed = card.task.status !== task.status;
      card.task = task;
      card.icon.textContent = icons[task.kind] || icons.other;
      card.description.textContent = task.description || task.id;
      card.status.textContent = statusLabels[task.status] || "Unknown";
      card.details.className = `task-card task-${Object.hasOwn(statusLabels, task.status) ? task.status : "other"}`;
      card.summaryText.textContent = task.summary || "";
      card.summaryText.hidden = !task.summary;
      updateMeta(card);
      if (changed) { cancel(card); if (card.details.open) void output(card); }
    }
    if (!tasks.length) list.append(textNode("p", "tasks-empty", "No background tasks."));
  };
}
