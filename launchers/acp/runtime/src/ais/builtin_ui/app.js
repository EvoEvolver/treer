"use strict";

(() => {
  const $ = (id) => document.getElementById(id);
  const validTheme = (theme) => theme === "light" || theme === "dark";
  const setTheme = (theme) => { document.documentElement.dataset.theme = theme; };
  const initialTheme = new URLSearchParams(location.search).get("theme");
  setTheme(validTheme(initialTheme) ? initialTheme : matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");
  window.addEventListener("message", (event) => {
    if (event.source === window.parent && event.data?.type === "treer:embed-theme" && validTheme(event.data.theme)) setTheme(event.data.theme);
  });

  // No leading slash: these must stay inside the Treer AIS tunnel prefix.
  async function api(path, options = {}) {
    const response = await fetch(path, { ...options, signal: options.signal || AbortSignal.timeout(15000) });
    const data = await response.json();
    if (!response.ok) throw new Error(data.error || `Request failed (${response.status})`);
    return data;
  }
  const post = (path, body) => api(path, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
  const fileUrl = (endpoint, path) => `${endpoint}?${new URLSearchParams({ path })}`;
  let selectedButton;
  let previewRequest;
  async function preview(node, button) {
    previewRequest?.abort();
    const request = new AbortController();
    previewRequest = request;
    selectedButton?.removeAttribute("aria-current");
    selectedButton = button;
    button.setAttribute("aria-current", "true");
    $("file-name").textContent = node.path;
    $("file-meta").textContent = "Loading…";
    $("file-content").textContent = "";
    $("truncated").hidden = true;
    try {
      const data = await api(fileUrl("v1/files", node.path), { signal: AbortSignal.any([request.signal, AbortSignal.timeout(15000)]) });
      if (previewRequest !== request) return;
      $("file-meta").textContent = `${data.size.toLocaleString()} bytes · ${data.language || "text"}`;
      $("file-content").textContent = data.content;
      $("truncated").hidden = !data.truncated;
    } catch (error) {
      if (previewRequest === request) $("file-meta").textContent = `Cannot preview: ${error.message}`;
    }
  }

  async function loadTree(path, list) {
    const data = await api(fileUrl("v1/files/tree", path));
    const fragment = document.createDocumentFragment();
    for (const node of data.entries) {
      const item = document.createElement("li");
      const button = document.createElement("button");
      button.type = "button";
      const directory = node.kind === "directory";
      button.textContent = `${directory ? "▸" : "·"} ${node.name}`;
      item.append(button);
      if (directory) {
        button.setAttribute("aria-expanded", "false");
        const children = document.createElement("ul");
        children.hidden = true;
        item.append(children);
        let loaded = false;
        let loading = false;
        button.addEventListener("click", async () => {
          if (loading) return;
          const opening = children.hidden;
          children.hidden = !opening;
          button.setAttribute("aria-expanded", String(opening));
          button.textContent = `${opening ? "▾" : "▸"} ${node.name}`;
          if (!opening || loaded) return;
          loading = true;
          try {
            await loadTree(node.path, children);
            loaded = true;
          } catch (error) {
            children.replaceChildren();
            const note = document.createElement("li");
            note.className = "note";
            note.textContent = `Cannot open folder: ${error.message}. Close and reopen to retry.`;
            children.append(note);
          } finally { loading = false; }
        });
      } else button.addEventListener("click", () => preview(node, button));
      fragment.append(item);
    }
    if (!data.entries.length) {
      const empty = document.createElement("li");
      empty.className = "note";
      empty.textContent = "Empty folder";
      fragment.append(empty);
    }
    list.replaceChildren(fragment);
  }
  async function refreshFiles() {
    $("refresh-files").disabled = true;
    $("tree-note").textContent = "Loading files…";
    try {
      await loadTree("", $("file-tree"));
      $("tree-note").textContent = "";
    } catch (error) { $("tree-note").textContent = error.message; }
    finally { $("refresh-files").disabled = false; }
  }
  $("refresh-files").addEventListener("click", refreshFiles);

  let busy = true;
  let sending = false;
  let aborting = false;
  let pendingPrompt;
  let transcriptSnapshot;
  function updateControls() {
    $("send").disabled = busy || sending || !$("prompt").value.trim();
    $("abort").disabled = !busy || sending || aborting;
  }
  function renderTranscript(entries) {
    const snapshot = JSON.stringify(entries);
    if (snapshot === transcriptSnapshot) return;
    transcriptSnapshot = snapshot;
    const transcript = $("transcript");
    const atBottom = transcript.scrollHeight - transcript.scrollTop - transcript.clientHeight < 48;
    const fragment = document.createDocumentFragment();
    for (const entry of entries) {
      const article = document.createElement("article");
      article.className = "entry";
      article.dataset.role = entry.role || entry.kind;
      const header = document.createElement("div");
      header.className = "entry-header";
      header.textContent = [entry.role || entry.kind, entry.created_at].filter(Boolean).join(" · ");
      const content = document.createElement("pre");
      content.className = "entry-content";
      content.textContent = typeof entry.content === "string" ? entry.content : JSON.stringify(entry.content, null, 2);
      article.append(header, content);
      fragment.append(article);
    }
    if (!entries.length) {
      const empty = document.createElement("p");
      empty.className = "note";
      empty.textContent = "No messages yet. Send a task to start a conversation.";
      fragment.append(empty);
    }
    transcript.replaceChildren(fragment);
    if (atBottom) transcript.scrollTop = transcript.scrollHeight;
  }
  async function pollStatus() {
    try {
      const data = await api("v1/status");
      busy = data.busy;
      $("agent-name").textContent = data.agent_id;
      $("status").textContent = `${data.status}${data.error ? ` · ${data.error}` : ""}`;
    } catch (error) {
      busy = true;
      $("status").textContent = `Disconnected · ${error.message}`;
    }
    updateControls();
  }
  async function pollTranscript() {
    try {
      let data = await api("v1/transcript?page=0&limit=100");
      if (data.next_page != null && data.page_count > 100) data = await api(`v1/transcript?page=${data.page_count - 100}&limit=100`);
      renderTranscript(data.entries);
      $("conversation-note").textContent = "";
    } catch (error) { $("conversation-note").textContent = `Transcript unavailable · ${error.message}`; }
  }
  async function poll() {
    await Promise.all([pollStatus(), pollTranscript()]);
    setTimeout(poll, document.hidden ? 5000 : 1200);
  }
  $("prompt").addEventListener("input", updateControls);
  $("prompt").addEventListener("keydown", (event) => {
    if (!event.isComposing && event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
      event.preventDefault();
      if (!$("send").disabled) $("prompt-form").requestSubmit();
    }
  });
  $("prompt-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    const text = $("prompt").value.trim();
    if (!text || busy || sending) return;
    // Retrying the same text after a lost response must not start another turn.
    if (pendingPrompt?.text !== text) pendingPrompt = { text, operation_id: crypto.randomUUID() };
    sending = true;
    $("prompt-note").textContent = "Sending…";
    updateControls();
    try {
      await post("v1/prompts", pendingPrompt);
      if ($("prompt").value.trim() === text) $("prompt").value = "";
      pendingPrompt = undefined;
      busy = true;
      $("prompt-note").textContent = "Accepted";
      await Promise.all([pollStatus(), pollTranscript()]);
    } catch (error) { $("prompt-note").textContent = `Send failed · ${error.message}. Retry to check acceptance.`; }
    finally { sending = false; updateControls(); }
  });
  $("abort").addEventListener("click", async () => {
    aborting = true;
    updateControls();
    try {
      await post("v1/abort", {});
      $("prompt-note").textContent = "Abort requested";
      await pollStatus();
    } catch (error) { $("prompt-note").textContent = `Abort failed · ${error.message}`; }
    finally { aborting = false; updateControls(); }
  });
  refreshFiles();
  poll();
})();
