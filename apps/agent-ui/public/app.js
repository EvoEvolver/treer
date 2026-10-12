import { renderMarkdown } from "./markdown.js";
import "./theme.js";
import { setupFileBrowser } from "./files.js";
import { setupTasks } from "./tasks.js";
import { setupGoal } from "./goal.js";

const elements = {
  composer: document.querySelector("#composer"),
  cwd: document.querySelector("#cwd"),
  effort: document.querySelector("#effort"),
  feedback: document.querySelector("#feedback"),
  interrupt: document.querySelector("#interrupt"),
  mark: document.querySelector("#brand-mark"),
  model: document.querySelector("#model"),
  prompt: document.querySelector("#prompt"),
  send: document.querySelector("#send"),
  queueSend: document.querySelector("#queue-send"),
  queue: document.querySelector("#prompt-queue"),
  shortcuts: document.querySelector("#prompt-shortcuts"),
  status: document.querySelector("#status"),
  timeline: document.querySelector("#timeline"),
  title: document.querySelector("#title"),
};

let snapshot = null;

function harnessName() {
  return snapshot?.harness?.name || "Agent";
}
let socket = null;
let reconnectTimer = null;
let streamConnected = false;
let submitting = false;
let loadingOlder = false;
let settingsBusy = false;
let localError = null;
let localNotice = null;
const renderTasks = setupTasks();
const goal = setupGoal({
  mutate: async (path, init) => {
    const payload = await request(path, init);
    snapshot = payload?.type === "state" || payload?.harness ? payload : await request("api/state");
    localError = null;
    render();
  },
  report: (message) => { localError = message; render(); },
  clearPrompt,
});

function node(tag, className, text) {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text != null) element.textContent = text;
  return element;
}

function markdownNode(className, text) {
  const element = node("div", `${className} markdown`);
  renderMarkdown(element, text || "");
  return element;
}

function itemNode(item) {
  if (!item?.text && item?.kind !== "reasoning") return null;
  if (item.kind === "userMessage") {
    const message = node("article", "message message-user");
    message.append(node("div", "message-label", "You"), markdownNode("message-copy", item.text));
    return message;
  }
  if (item.kind === "agentMessage") {
    const message = node("article", "message message-assistant");
    message.append(markdownNode("message-copy", item.text));
    return message;
  }
  if (item.kind === "reasoning") {
    const details = node("details", "reasoning");
    details.append(node("summary", "", "Reasoning"), markdownNode("reasoning-copy", item.text));
    return details;
  }
  if (item.kind === "plan") {
    const card = node("section", "plan-card");
    card.append(node("strong", "", "Plan"));
    if (item.text && item.text !== "Plan") card.append(node("p", "plan-explanation", item.text));
    const list = node("ul", "plan-steps");
    for (const step of item.steps ?? []) {
      const row = node("li", `plan-${step.status}`);
      const check = node("span", "plan-check", step.status === "completed" ? "✓" : step.status === "inProgress" ? "◉" : "○");
      check.setAttribute("aria-label", step.status === "completed" ? "Completed" : step.status === "inProgress" ? "In progress" : "Pending");
      row.append(check, node("span", "", step.step));
      list.append(row);
    }
    card.append(list);
    return card;
  }
  if (["commandExecution", "fileChange", "toolCall"].includes(item.kind)) {
    const details = node("details", `tool tool-${item.status || "complete"}`);
    const summary = node("summary", "tool-summary");
    summary.append(
      node("span", "tool-icon", item.kind === "commandExecution" ? ">_" : item.kind === "fileChange" ? "+-" : "<>"),
      node("span", "tool-name", item.text || item.kind),
      node("span", "tool-status", item.status || "done"),
    );
    details.append(summary);
    if (item.kind === "fileChange") {
      const body = node("div", "changes-body");
      for (const change of item.changes ?? []) {
        const file = node("section", "file-diff");
        file.append(node("h3", "diff-path", `${change.kind} · ${change.path}`));
        const diff = node("pre", "diff-code");
        for (const line of change.diff.split("\n")) {
          const kind = line.startsWith("@@") ? "hunk" : line.startsWith("+++") || line.startsWith("---") ? "header" : line.startsWith("+") ? "add" : line.startsWith("-") ? "remove" : "context";
          diff.append(node("span", `diff-line diff-${kind}`, line + "\n"));
        }
        if (!change.diff) diff.textContent = "No diff was provided.";
        file.append(diff);
        body.append(file);
      }
      if (!item.changes?.length) body.append(node("p", "file-notice", "No file details were provided."));
      details.append(body);
    }
    if (item.detailText && item.detailText !== item.text) {
      const body = node("div", "tool-body");
      body.append(node("pre", "", item.detailText));
      details.append(body);
    }
    return details;
  }
  const details = node("details", "tool");
  details.append(node("summary", "tool-summary", item.kind || "Event"), node("pre", "tool-plain", item.text));
  return details;
}

function workingNode() {
  const activity = node("div", "agent-activity");
  activity.setAttribute("role", "status");
  activity.setAttribute("aria-label", `${harnessName()} is working`);
  activity.append(node("span", "activity-spinner"), node("span", "", "Working"));
  return activity;
}

function loadOlderNode() {
  const row = node("div", "history-control");
  const button = node("button", "history-button", loadingOlder ? "Loading..." : "Load earlier messages");
  button.type = "button";
  button.disabled = loadingOlder;
  button.addEventListener("click", () => void loadOlderMessages());
  row.append(button);
  return row;
}

function renderTimeline(scrollAnchor) {
  const opened = new Set([...elements.timeline.querySelectorAll("details[open]")].map((detail) => detail.dataset.itemId));
  const nearBottom = elements.timeline.scrollHeight - elements.timeline.scrollTop - elements.timeline.clientHeight < 120;
  const fragment = document.createDocumentFragment();
  if (snapshot?.thread?.hasOlderItems) fragment.append(loadOlderNode());
  const shownGoals = new Set();
  function goalNotices(turnId) {
    for (const notice of goal.notices) {
      if (shownGoals.has(notice) || (turnId != null && notice.afterTurn !== turnId)) continue;
      const completion = node("section", "goal-completion", `✓ Goal complete: ${notice.objective}`);
      completion.setAttribute("role", "status");
      fragment.append(completion);
      shownGoals.add(notice);
    }
  }
  for (const turn of snapshot?.thread?.turns ?? []) {
    for (const item of turn.items ?? []) {
      const rendered = itemNode(item);
      if (rendered?.tagName === "DETAILS") {
        rendered.dataset.itemId = `${turn.id}:${item.id}`;
        rendered.open = opened.has(rendered.dataset.itemId);
      }
      if (rendered) fragment.append(rendered);
    }
    if (turn.error) fragment.append(node("div", "turn-error", turn.error));
    goalNotices(turn.id);
  }
  goalNotices(null);
  if (snapshot?.thread?.status === "running") fragment.append(workingNode());
  elements.timeline.replaceChildren(fragment);
  if (!elements.timeline.childElementCount) {
    const empty = node("div", "empty");
    empty.append(node("span", "empty-mark", harnessName().charAt(0).toUpperCase()), node("strong", "", snapshot?.ready ? "Ready" : "Starting"));
    elements.timeline.append(empty);
  }
  if (scrollAnchor) {
    elements.timeline.scrollTop = scrollAnchor.top + elements.timeline.scrollHeight - scrollAnchor.height;
  } else if (nearBottom) {
    elements.timeline.scrollTop = elements.timeline.scrollHeight;
  }
}

function replaceOptions(select, options, value, placeholder) {
  const signature = JSON.stringify([options, value]);
  if (select.dataset.signature === signature) return;
  select.dataset.signature = signature;
  select.replaceChildren();
  if (!options.length) select.append(new Option(placeholder, ""));
  // A harness may leave a setting on its own default, which is not one of the choices.
  else if (!options.some((option) => option.value === value)) select.append(new Option("Default", ""));
  for (const option of options) select.append(new Option(option.label, option.value));
  select.value = value || "";
}

function renderSettings() {
  const thread = snapshot?.thread;
  const models = snapshot?.modelOptions ?? [];
  replaceOptions(
    elements.model,
    models.map((option) => ({ label: option.displayName || option.model, value: option.model })),
    thread?.model,
    "Model",
  );
  const selected = models.find((option) => option.model === thread?.model);
  const efforts = selected?.supportedReasoningEfforts ?? [];
  replaceOptions(
    elements.effort,
    efforts.map((option) => ({ label: option.reasoningEffort, value: option.reasoningEffort })),
    thread?.reasoningEffort,
    "Effort",
  );
  elements.model.disabled = settingsBusy || !thread || models.length === 0;
  elements.effort.disabled = settingsBusy || !thread || efforts.length === 0;
  // Harnesses without reasoning levels get no empty picker.
  elements.effort.closest(".select-wrap").hidden = Boolean(thread) && efforts.length === 0;
}

function render(scrollAnchor) {
  const thread = snapshot?.thread;
  const state = !streamConnected ? "reconnecting" : snapshot?.runtime?.state ?? "starting";
  const name = harnessName();
  elements.title.textContent = thread?.title || name;
  document.title = name;
  elements.mark.textContent = name.charAt(0).toUpperCase();
  elements.prompt.placeholder = `Message ${name}`;
  elements.prompt.setAttribute("aria-label", `Message ${name}`);
  elements.cwd.textContent = thread?.cwd || "Connecting";
  elements.status.className = `status status-${state}`;
  elements.status.lastElementChild.textContent = state.charAt(0).toUpperCase() + state.slice(1);
  elements.interrupt.disabled = thread?.status !== "running";
  renderComposer();
  elements.feedback.hidden = !localError && !localNotice;
  elements.feedback.classList.toggle("feedback-notice", !localError);
  elements.feedback.textContent = localError || localNotice || "";
  renderTasks(snapshot);
  renderQueue();
  goal.render(snapshot);
  renderSettings();
  renderTimeline(scrollAnchor);
}

function renderComposer() {
  const canSend = !submitting && snapshot?.ready && elements.prompt.value.trim().length > 0;
  const steering = snapshot?.thread?.status === "running";
  elements.send.disabled = !canSend;
  const canSteer = Boolean(snapshot?.harness?.capabilities?.steer);
  const label = steering ? canSteer ? "Steer current run" : "Queue message (steering unavailable)" : "Send message";
  elements.send.setAttribute("aria-label", label);
  elements.send.title = label;
  elements.send.textContent = steering ? canSteer ? "Steer" : "Queue" : "↑";
  elements.send.classList.toggle("send-labeled", steering);
  elements.queueSend.hidden = !steering || !canSteer;
  elements.queueSend.disabled = !canSend;
  elements.shortcuts.textContent = steering
    ? `${canSteer ? "Enter to steer" : "Enter to queue (steering unavailable)"} · Tab to queue · Shift+Enter for a new line`
    : "Enter to send · Tab to queue · Shift+Enter for a new line";
}

function renderQueue() {
  const queue = snapshot?.queue ?? [];
  const focused = elements.queue.contains(document.activeElement) ? document.activeElement.dataset.queueId : null;
  elements.queue.hidden = !queue.length;
  elements.queue.replaceChildren();
  for (const entry of queue) {
    const row = node("div", "queued-prompt");
    const cancel = node("button", "", "Cancel");
    cancel.type = "button";
    cancel.dataset.queueId = entry.id;
    cancel.setAttribute("aria-label", `Cancel queued message: ${entry.text}`);
    cancel.addEventListener("click", async () => {
      cancel.disabled = true;
      try {
        const payload = await request(`api/queue/${encodeURIComponent(entry.id)}`, { method: "DELETE" });
        snapshot = payload?.harness || payload?.type === "state" ? payload : await request("api/state");
        localError = null;
      } catch (error) { localError = error.message; }
      render();
    });
    row.append(node("span", "queue-label", "Queued"), node("span", "queued-text", entry.text), cancel);
    elements.queue.append(row);
  }
  if (focused) [...elements.queue.querySelectorAll("button")].find((button) => button.dataset.queueId === focused)?.focus();
}

function clearPrompt(expected) {
  if (elements.prompt.value.trim() !== expected) return;
  elements.prompt.value = "";
  elements.prompt.style.height = "auto";
  renderComposer();
}

async function request(path, init) {
  const response = await fetch(path, {
    ...init,
    headers: { "content-type": "application/json", ...(init?.headers ?? {}) },
  });
  const payload = response.status === 204 ? null : await response.json();
  if (!response.ok) throw new Error(payload?.error?.message || payload?.error || `HTTP ${response.status}`);
  return payload;
}

async function updateSettings(input) {
  settingsBusy = true;
  localError = null;
  render();
  try {
    snapshot = await request("api/settings", {
      method: "POST",
      body: JSON.stringify({ ...input, history_limit: snapshot?.history?.limit ?? 50 }),
    });
  } catch (error) {
    localError = error.message;
  } finally {
    settingsBusy = false;
    render();
  }
}

elements.model.addEventListener("change", () => {
  void updateSettings({ model: elements.model.value });
});

elements.effort.addEventListener("change", () => {
  void updateSettings({ reasoningEffort: elements.effort.value || null });
});

async function submitPrompt(mode = "steer") {
  const prompt = elements.prompt.value.trim();
  if (!prompt || submitting || !snapshot?.ready) return;
  submitting = true;
  localError = null;
  localNotice = null;
  render();
  try {
    const command = await goal.intercept(prompt);
    if (command) { if (command.clear) clearPrompt(prompt); return; }
    const downgrade = mode === "steer" && snapshot?.thread?.status === "running" && !snapshot?.harness?.capabilities?.steer;
    snapshot = await request("api/prompt", {
      method: "POST",
      body: JSON.stringify({ prompt, mode: downgrade ? "queue" : mode, history_limit: snapshot?.history?.limit ?? 50 }),
    });
    if (downgrade || snapshot.submission?.downgraded) localNotice = "This harness does not support steering; your message was queued.";
    clearPrompt(prompt);
  } catch (error) {
    localError = error.message;
  } finally {
    submitting = false;
    render();
  }
}
elements.composer.addEventListener("submit", (event) => { event.preventDefault(); void submitPrompt(); });
elements.queueSend.addEventListener("click", () => void submitPrompt("queue"));

elements.prompt.addEventListener("keydown", (event) => {
  if (event.isComposing) return;
  if (event.key === "Tab" && !event.shiftKey && elements.prompt.value.trim()) {
    event.preventDefault();
    void submitPrompt("queue");
  } else if (event.key === "Enter" && !event.shiftKey) {
    event.preventDefault();
    elements.composer.requestSubmit();
  }
});

elements.prompt.addEventListener("input", () => {
  elements.prompt.style.height = "auto";
  elements.prompt.style.height = `${Math.min(elements.prompt.scrollHeight, 180)}px`;
  renderComposer();
});

async function loadOlderMessages() {
  if (loadingOlder || !snapshot?.thread?.hasOlderItems) return;
  const scrollAnchor = {
    height: elements.timeline.scrollHeight,
    top: elements.timeline.scrollTop,
  };
  loadingOlder = true;
  const button = elements.timeline.querySelector(".history-button");
  if (button) {
    button.disabled = true;
    button.textContent = "Loading...";
  }
  localError = null;
  try {
    snapshot = await request("api/history/older", {
      method: "POST",
      body: JSON.stringify({ limit: snapshot?.history?.limit ?? 50 }),
    });
    loadingOlder = false;
    if (socket?.readyState === WebSocket.OPEN) {
      socket.send(JSON.stringify({
        type: "history-limit",
        limit: snapshot.history.limit,
        respond: false,
      }));
    }
    render(scrollAnchor);
  } catch (error) {
    loadingOlder = false;
    localError = error.message;
    render();
  } finally {
    renderComposer();
  }
}

elements.interrupt.addEventListener("click", async () => {
  localError = null;
  try {
    snapshot = await request("api/interrupt", {
      method: "POST",
      body: JSON.stringify({ history_limit: snapshot?.history?.limit ?? 50 }),
    });
  } catch (error) {
    localError = error.message;
  }
  render();
});

function connect() {
  clearTimeout(reconnectTimer);
  const url = new URL("ws", document.baseURI);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  socket = new WebSocket(url);
  socket.addEventListener("open", () => {
    streamConnected = true;
    if ((snapshot?.history?.limit ?? 50) > 50) {
      socket.send(JSON.stringify({ type: "history-limit", limit: snapshot.history.limit }));
    }
    render();
  });
  socket.addEventListener("message", (event) => {
    try {
      const payload = JSON.parse(String(event.data));
      if (payload?.type === "state") snapshot = payload;
      streamConnected = true;
      render();
    } catch {
      // Ignore malformed frames.
    }
  });
  socket.addEventListener("close", () => {
    streamConnected = false;
    render();
    reconnectTimer = setTimeout(connect, 1000);
  });
}

request("api/state")
  .then((payload) => {
    snapshot = payload;
    render();
  })
  .catch((error) => {
    localError = error.message;
    render();
  });
setupFileBrowser(request);
connect();
