import { renderMarkdown } from "./markdown.js";

const elements = {
  composer: document.querySelector("#composer"),
  cwd: document.querySelector("#cwd"),
  effort: document.querySelector("#effort"),
  feedback: document.querySelector("#feedback"),
  interrupt: document.querySelector("#interrupt"),
  model: document.querySelector("#model"),
  prompt: document.querySelector("#prompt"),
  send: document.querySelector("#send"),
  status: document.querySelector("#status"),
  timeline: document.querySelector("#timeline"),
  title: document.querySelector("#title"),
};

let snapshot = null;
let socket = null;
let reconnectTimer = null;
let streamConnected = false;
let submitting = false;
let loadingOlder = false;
let settingsBusy = false;
let localError = null;

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
  if (["commandExecution", "fileChange", "toolCall"].includes(item.kind)) {
    const details = node("details", `tool tool-${item.status || "complete"}`);
    const summary = node("summary", "tool-summary");
    summary.append(
      node("span", "tool-icon", item.kind === "commandExecution" ? ">_" : item.kind === "fileChange" ? "+-" : "<>"),
      node("span", "tool-name", item.text || item.kind),
      node("span", "tool-status", item.status || "done"),
    );
    details.append(summary);
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
  activity.setAttribute("aria-label", "Codex is working");
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
  const nearBottom = elements.timeline.scrollHeight - elements.timeline.scrollTop - elements.timeline.clientHeight < 120;
  const fragment = document.createDocumentFragment();
  if (snapshot?.thread?.hasOlderItems) fragment.append(loadOlderNode());
  for (const turn of snapshot?.thread?.turns ?? []) {
    for (const item of turn.items ?? []) {
      const rendered = itemNode(item);
      if (rendered) fragment.append(rendered);
    }
    if (turn.error) fragment.append(node("div", "turn-error", turn.error));
  }
  if (snapshot?.thread?.status === "running") fragment.append(workingNode());
  elements.timeline.replaceChildren(fragment);
  if (!elements.timeline.childElementCount) {
    const empty = node("div", "empty");
    empty.append(node("span", "empty-mark", "C"), node("strong", "", snapshot?.ready ? "Ready" : "Starting"));
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
}

function render(scrollAnchor) {
  const thread = snapshot?.thread;
  const state = !streamConnected ? "reconnecting" : snapshot?.runtime?.state ?? "starting";
  elements.title.textContent = thread?.title || "Codex";
  elements.cwd.textContent = thread?.cwd || "Connecting";
  elements.status.className = `status status-${state}`;
  elements.status.lastElementChild.textContent = state.charAt(0).toUpperCase() + state.slice(1);
  elements.interrupt.disabled = thread?.status !== "running";
  renderComposer();
  elements.feedback.hidden = !localError;
  elements.feedback.textContent = localError || "";
  renderSettings();
  renderTimeline(scrollAnchor);
}

function renderComposer() {
  const canSend = !submitting && snapshot?.ready && elements.prompt.value.trim().length > 0;
  const steering = snapshot?.thread?.status === "running";
  elements.send.disabled = !canSend;
  elements.send.setAttribute("aria-label", steering ? "Steer current run" : "Send message");
  elements.send.title = steering ? "Steer current run" : "Send message";
}

async function request(path, init) {
  const response = await fetch(path, {
    ...init,
    headers: { "content-type": "application/json", ...(init?.headers ?? {}) },
  });
  const payload = await response.json();
  if (!response.ok) throw new Error(payload.error || `HTTP ${response.status}`);
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
  void updateSettings({ reasoningEffort: elements.effort.value });
});

elements.composer.addEventListener("submit", async (event) => {
  event.preventDefault();
  const prompt = elements.prompt.value.trim();
  if (!prompt || submitting || !snapshot?.ready) return;
  submitting = true;
  localError = null;
  render();
  try {
    snapshot = await request("api/prompt", {
      method: "POST",
      body: JSON.stringify({ prompt, history_limit: snapshot?.history?.limit ?? 50 }),
    });
    elements.prompt.value = "";
    elements.prompt.style.height = "auto";
  } catch (error) {
    localError = error.message;
  } finally {
    submitting = false;
    render();
  }
});

elements.prompt.addEventListener("keydown", (event) => {
  if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
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
connect();
