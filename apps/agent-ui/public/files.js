export function setupFileBrowser(request) {
  const $ = (id) => document.getElementById(id);
  const browser = $("file-browser");
  const tree = $("file-tree");
  const toggle = $("files-toggle");
  const expanded = new Set([""]);
  const cache = new Map();
  const pending = new Set();
  const errors = new Map();
  let selected = null;
  let selection = 0;

  function element(tag, className, text) {
    const node = document.createElement(tag);
    node.className = className;
    if (text != null) node.textContent = text;
    return node;
  }
  function previewTab(show) {
    browser.classList.toggle("show-preview", show);
    $("files-browse").setAttribute("aria-pressed", String(!show));
    $("files-preview-tab").setAttribute("aria-pressed", String(show));
  }
  async function load(path, refresh = false) {
    if (pending.has(path) || (!refresh && cache.has(path))) return;
    pending.add(path);
    errors.delete(path);
    render();
    try { cache.set(path, (await request(`v1/files/tree?path=${encodeURIComponent(path)}`)).entries); }
    catch (error) { errors.set(path, error.message); }
    finally { pending.delete(path); render(); }
  }
  async function preview(path) {
    selected = path;
    const version = ++selection;
    $("file-path").textContent = path;
    $("file-meta").textContent = "Loading…";
    $("file-meta").classList.remove("truncated");
    $("file-content").textContent = "";
    $("file-error").hidden = true;
    $("files-preview-tab").disabled = false;
    previewTab(true);
    render();
    try {
      const file = await request(`v1/files?path=${encodeURIComponent(path)}`);
      if (version !== selection) return;
      $("file-content").textContent = file.content;
      $("file-meta").textContent = `${file.size.toLocaleString()} bytes · ${file.truncated ? "Truncated to 64 KiB" : "Read-only"}`;
      $("file-meta").classList.toggle("truncated", file.truncated);
    } catch (error) {
      if (version !== selection) return;
      $("file-meta").textContent = "Preview unavailable";
      $("file-error").hidden = false;
      $("file-error").textContent = error.message;
    }
  }
  function children(path, depth) {
    const group = element("ul", "file-children");
    for (const file of cache.get(path) ?? []) {
      const row = element("li", "file-entry");
      const button = element("button", `file-row${selected === file.path ? " selected" : ""}`);
      button.type = "button";
      button.style.paddingLeft = `${12 + depth * 16}px`;
      button.title = file.path;
      const directory = file.kind === "directory";
      if (directory) button.setAttribute("aria-expanded", String(expanded.has(file.path)));
      else if (selected === file.path) button.setAttribute("aria-current", "true");
      const icon = element("span", "file-icon", directory ? expanded.has(file.path) ? "▾" : "▸" : "·");
      icon.setAttribute("aria-hidden", "true");
      button.append(icon, element("span", "file-name", file.name));
      button.addEventListener("click", () => {
        if (!directory) { void preview(file.path); return; }
        if (expanded.has(file.path)) expanded.delete(file.path);
        else { expanded.add(file.path); void load(file.path); }
        render();
      });
      row.append(button);
      if (directory && expanded.has(file.path)) row.append(children(file.path, depth + 1));
      group.append(row);
    }
    if (pending.has(path)) group.append(element("li", "file-notice", "Loading…"));
    else if (errors.has(path)) {
      const row = element("li", "file-notice", errors.get(path));
      const retry = element("button", "", "Retry");
      retry.addEventListener("click", () => void load(path, true));
      row.append(retry);
      group.append(row);
    } else if (cache.has(path) && !cache.get(path).length) group.append(element("li", "file-notice", "Empty folder"));
    return group;
  }
  function render() {
    const focused = tree.contains(document.activeElement) ? document.activeElement.title : null;
    tree.replaceChildren(children("", 0));
    if (focused !== null) [...tree.querySelectorAll("button")].find((button) => button.title === focused)?.focus();
  }
  function open(show) {
    browser.hidden = !show;
    toggle.setAttribute("aria-expanded", String(show));
    if (show) { void load(""); $("files-close").focus(); }
    else toggle.focus();
  }
  toggle.addEventListener("click", () => open(browser.hidden));
  $("files-close").addEventListener("click", () => open(false));
  $("files-refresh").addEventListener("click", () => { for (const path of expanded) void load(path, true); if (selected) void preview(selected); });
  $("files-browse").addEventListener("click", () => previewTab(false));
  $("files-preview-tab").addEventListener("click", () => previewTab(true));
  browser.addEventListener("keydown", (event) => { if (event.key === "Escape") { event.preventDefault(); open(false); } });
}
