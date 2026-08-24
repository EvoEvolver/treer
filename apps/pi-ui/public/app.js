import { renderMarkdown } from "./markdown.js";

const elements = {
  abort: document.querySelector("#abort"),
  compact: document.querySelector("#compact"),
  composer: document.querySelector("#composer"),
  cwd: document.querySelector("#cwd"),
  delivery: document.querySelector("#delivery"),
  feedback: document.querySelector("#feedback"),
  fork: document.querySelector("#fork"),
  model: document.querySelector("#model"),
  prompt: document.querySelector("#prompt"),
  send: document.querySelector("#send"),
  status: document.querySelector("#status"),
  timeline: document.querySelector("#timeline"),
};

let snapshot = null;
let streamConnected = false;
let submitting = false;
let localError = null;
let localNotice = null;
let forking = false;

function textContent(content) {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .filter((part) => part?.type === "text")
    .map((part) => part.text ?? "")
    .join("\n");
}

function resultText(result) {
  const content = result?.content;
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return result == null ? "" : JSON.stringify(result, null, 2);
  return content.map((part) => part?.text ?? "").filter(Boolean).join("\n");
}

function node(tag, className, text) {
  const item = document.createElement(tag);
  if (className) item.className = className;
  if (text != null) item.textContent = text;
  return item;
}

function markdownNode(className, text) {
  const item = node("div", `${className} markdown`);
  renderMarkdown(item, text);
  return item;
}

function messageCard(message, id, hiddenToolIds = new Set()) {
  const role = message.role;
  const card = node("article", `message message-${role}`);
  card.dataset.id = id;
  if (role === "user") {
    card.append(node("div", "message-label", "You"), markdownNode("message-copy", textContent(message.content)));
    return card;
  }
  if (role === "assistant") {
    const copy = textContent(message.content);
    if (copy) card.append(markdownNode("message-copy", copy));
    const thinking = Array.isArray(message.content)
      ? message.content.filter((part) => part?.type === "thinking").map((part) => part.thinking ?? "").join("\n")
      : "";
    if (thinking) {
      const details = node("details", "thinking");
      details.append(node("summary", "", "Reasoning"), node("pre", "", thinking));
      card.append(details);
    }
    for (const part of Array.isArray(message.content) ? message.content : []) {
      if (part?.type === "toolCall" && !hiddenToolIds.has(part.id)) {
        card.append(toolCard(part.id, part.name, part.arguments, null, "queued"));
      }
    }
    return card.childElementCount ? card : null;
  }
  if (role === "toolResult") {
    return toolCard(message.toolCallId, message.toolName, null, message, message.isError ? "error" : "success");
  }
  card.append(markdownNode("message-copy", textContent(message.content)));
  return card;
}

function toolCard(id, name, args, result, status) {
  const details = node("details", `tool tool-${status}`);
  details.dataset.toolId = id ?? "";
  const summary = node("summary", "tool-summary");
  summary.append(node("span", "tool-icon", toolIcon(name)), node("span", "tool-name", name || "tool"), node("span", "tool-status", status));
  details.append(summary);
  const body = node("div", "tool-body");
  if (args != null) body.append(node("pre", "tool-input", JSON.stringify(args, null, 2)));
  const output = resultText(result);
  if (output) body.append(node("pre", "tool-output", output));
  details.append(body);
  return details;
}

function toolIcon(name = "") {
  if (/bash|shell|run|exec/i.test(name)) return ">_";
  if (/read|grep|find|ls/i.test(name)) return "≡";
  if (/write|edit|patch/i.test(name)) return "±";
  return "·";
}

function renderTimeline() {
  const wasNearBottom = elements.timeline.scrollHeight - elements.timeline.scrollTop - elements.timeline.clientHeight < 120;
  const fragment = document.createDocumentFragment();
  const renderedToolIds = new Set((snapshot?.activeTools ?? []).map((tool) => tool.id));
  for (const entry of snapshot?.entries ?? []) {
    if (entry.type === "message" && entry.message?.role === "toolResult") {
      renderedToolIds.add(entry.message.toolCallId);
    }
  }
  for (const entry of snapshot?.entries ?? []) {
    if (entry.type === "message") {
      const card = messageCard(entry.message, entry.id, renderedToolIds);
      if (card) fragment.append(card);
    } else if (entry.type === "compaction" || entry.type === "branch_summary") {
      const card = node("article", "summary-card");
      card.append(node("div", "message-label", entry.type === "compaction" ? "Compacted context" : "Branch summary"));
      card.append(markdownNode("message-copy", entry.summary));
      fragment.append(card);
    }
  }
  if (snapshot?.liveMessage) {
    const live = messageCard(snapshot.liveMessage, "live", renderedToolIds);
    if (live) {
      live.classList.add("message-live");
      fragment.append(live);
    }
  }
  for (const tool of snapshot?.activeTools ?? []) {
    fragment.append(toolCard(tool.id, tool.name, tool.args, tool.result, tool.status));
  }
  elements.timeline.replaceChildren(fragment);
  if (!elements.timeline.childElementCount) {
    const empty = node("div", "empty");
    empty.append(node("strong", "", "Start a conversation"), node("span", "", "Pi is ready in this workspace."));
    elements.timeline.append(empty);
  }
  if (wasNearBottom) elements.timeline.scrollTop = elements.timeline.scrollHeight;
}

function render() {
  elements.cwd.textContent = snapshot?.cwd ?? "Connecting";
  elements.model.textContent = snapshot?.model
    ? `${snapshot.model.name || snapshot.model.id}${snapshot.thinkingLevel ? ` · ${snapshot.thinkingLevel}` : ""}`
    : "No model";
  const statusLabel = !streamConnected ? "Reconnecting" : snapshot?.busy ? "Working" : snapshot?.connected ? "Ready" : "Starting";
  elements.status.className = `status status-${statusLabel.toLowerCase()}`;
  elements.status.lastElementChild.textContent = statusLabel;
  elements.abort.disabled = !snapshot?.busy;
  elements.send.disabled = submitting || !snapshot?.connected;
  elements.compact.disabled = snapshot?.busy || !snapshot?.connected;
  elements.fork.disabled = forking || snapshot?.forking || snapshot?.busy || !snapshot?.canFork;
  elements.fork.textContent = forking || snapshot?.forking ? "Forking..." : "Fork";
  const error = localError ?? snapshot?.error;
  const feedback = error ?? localNotice;
  elements.feedback.hidden = !feedback;
  elements.feedback.className = `feedback ${error ? "feedback-error" : "feedback-success"}`;
  elements.feedback.textContent = feedback ?? "";
  renderTimeline();
}

async function post(path, body) {
  const response = await fetch(path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: body == null ? undefined : JSON.stringify(body),
  });
  const payload = await response.json();
  if (!response.ok) throw new Error(payload.error || `HTTP ${response.status}`);
  return payload;
}

elements.composer.addEventListener("submit", async (event) => {
  event.preventDefault();
  const message = elements.prompt.value.trim();
  if (!message || submitting) return;
  submitting = true;
  localError = null;
  localNotice = null;
  render();
  try {
    await post("api/prompt", { message, mode: elements.delivery.value });
    elements.prompt.value = "";
    elements.prompt.style.height = "auto";
  } catch (error) {
    localError = error.message;
  } finally {
    submitting = false;
    render();
  }
});

elements.prompt.addEventListener("input", () => {
  elements.prompt.style.height = "auto";
  elements.prompt.style.height = `${Math.min(elements.prompt.scrollHeight, 180)}px`;
});

elements.abort.addEventListener("click", async () => {
  localError = null;
  localNotice = null;
  try { await post("api/abort"); } catch (error) { localError = error.message; }
  render();
});

elements.compact.addEventListener("click", async () => {
  localError = null;
  localNotice = null;
  try { await post("api/compact"); } catch (error) { localError = error.message; }
  render();
});

elements.fork.addEventListener("click", async () => {
  if (forking || snapshot?.forking) return;
  forking = true;
  localError = null;
  localNotice = null;
  render();
  try {
    const result = await post("api/fork");
    localNotice = `Created ${result.agent.name}. It is ready in the Agents list.`;
  } catch (error) {
    localError = error.message;
  } finally {
    forking = false;
    render();
  }
});

const source = new EventSource("api/events");
source.addEventListener("open", () => { streamConnected = true; render(); });
source.addEventListener("error", () => { streamConnected = false; render(); });
source.addEventListener("snapshot", (event) => {
  snapshot = JSON.parse(event.data);
  streamConnected = true;
  render();
});

fetch("api/snapshot").then((response) => response.json()).then((value) => { snapshot = value; render(); }).catch(() => {});
