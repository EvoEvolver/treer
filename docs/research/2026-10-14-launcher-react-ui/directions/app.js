// Mock-only behaviour for the static prototypes. The shipped UI is React.
(() => {
  "use strict";
  const $ = (s, r = document) => r.querySelector(s);
  const $$ = (s, r = document) => [...r.querySelectorAll(s)];
  const app = $("#app"), root = document.documentElement;

  // Appearance (look + theme) lives in boot.js; the UI only reflects it.
  const A = window.TreerAppearance;
  A.subscribe(({ look, mode, locked }) => {
    $$("[data-look-set]").forEach((b) => { const on = b.dataset.lookSet === look; b.setAttribute("aria-checked", String(on)); b.setAttribute("aria-pressed", String(on)); });
    $$("[data-theme-set]").forEach((b) => { const on = b.dataset.themeSet === mode; b.setAttribute("aria-checked", String(on)); b.setAttribute("aria-pressed", String(on)); });
    $$("[data-act=look]").forEach((b) => { b.hidden = locked; });
  });
  function setMenu(open) { $("#menu").hidden = !open; $$("[data-act=look]").forEach((b) => b.setAttribute("aria-expanded", String(open))); if (open) $("#menu [aria-checked=true]")?.focus({ preventScroll: true }); }

  // Views
  function setView(v) { app.dataset.view = v; $$("[data-view-set]").forEach((b) => b.setAttribute("aria-pressed", String(b.dataset.viewSet === v))); }
  // Panels (files / tasks)
  function setPanel(name, open) {
    const p = $("#" + name); if (!p) return;
    p.hidden = !open;
    if (open && innerWidth <= 900) $$(".panel").forEach((o) => { if (o !== p) { o.hidden = true; syncPanel(o); } });
    syncPanel(p);
  }
  function syncPanel(p) { $$(`[data-act="${p.id}"]`).forEach((b) => b.setAttribute("aria-expanded", String(!p.hidden))); }
  // Popovers: one at a time; Esc/close returns focus to its segment.
  let lastSeg = null;
  function pop(name, focusBack) {
    $$(".seg").forEach((b) => b.setAttribute("aria-expanded", String(b.dataset.pop === name)));
    $$(".pop").forEach((p) => { p.hidden = p.id !== "pop-" + name; });
    if (name) $("#pop-" + name + " .xbtn")?.focus({ preventScroll: true });
    else if (focusBack && lastSeg) lastSeg.focus();
  }
  document.addEventListener("click", (e) => {
    const t = e.target.closest("button, a"); if (!t) { if (!$("#menu").hidden && !e.target.closest("#menu")) setMenu(false); return; }
    if (t.dataset.act === "files" || t.dataset.act === "tasks") { const p = $("#" + t.dataset.act); setPanel(t.dataset.act, p.hidden); }
    else if (t.dataset.act === "theme") A.toggleTheme();
    else if (t.dataset.act === "look") setMenu($("#menu").hidden);
    else if (t.hasAttribute("data-close-menu")) { setMenu(false); $$("[data-act=look]").find((b) => b.offsetParent)?.focus(); }
    else if (t.dataset.lookSet) A.setLook(t.dataset.lookSet);
    else if (t.dataset.close) setPanel(t.dataset.close, false);
    else if (t.dataset.pop) { lastSeg = t; pop(t.getAttribute("aria-expanded") === "true" ? "" : t.dataset.pop, true); }
    else if (t.dataset.popSet) { lastSeg = null; pop(t.dataset.popSet); }
    else if (t.hasAttribute("data-close-pop")) pop("", true);
    else if (t.dataset.themeSet) A.setTheme(t.dataset.themeSet);
    else if (t.dataset.viewSet) setView(t.dataset.viewSet);
    else if (t.classList.contains("sum")) { const g = t.closest(".grp"), o = g.dataset.open !== "true"; g.dataset.open = String(o); t.setAttribute("aria-expanded", String(o)); }
    else if (t.dataset.ftab) { $("#files").dataset.ftab = t.dataset.ftab; $$("[data-ftab]").forEach((b) => b.setAttribute("aria-pressed", String(b === t))); }
    else if (t.id === "jump") thread.scrollTo({ top: thread.scrollHeight });
    else if (t.id === "pv-exp") { forceExp = !forceExp; syncExp(); }
    else if (!t.closest(".pop") && !t.closest(".strip") && $$(".pop").some((p) => !p.hidden) && !t.closest(".pv")) pop("");
    if (!$("#menu").hidden && !t.closest("#menu") && t.dataset.act !== "look") setMenu(false);
  });
  document.addEventListener("keydown", (e) => { if (e.key === "Escape") { if (!$("#menu").hidden) { setMenu(false); $$("[data-act=look]").find((b) => b.offsetParent)?.focus(); } else if ($$(".pop").some((p) => !p.hidden)) pop("", true); else if (!$("#files").hidden && innerWidth <= 900) setPanel("files", false); } });
  $("#pv").open = innerWidth > 700;

  // Composer: one real textarea; compact when idle, expanded when focused or multi-line.
  const ta = $("#ta"), box = $("#box"), composer = $("#composer"), send = $("#send"), thread = $("#thread");
  let forceExp = false;
  function size() { ta.style.height = "auto"; ta.style.height = Math.min(ta.scrollHeight, 180) + "px"; }
  function syncExp() {
    const exp = forceExp || document.activeElement === ta || ta.value.includes("\n") || ta.value.length > 70;
    box.classList.toggle("exp", exp); composer.classList.toggle("exp", exp);
    $("#pv-exp")?.setAttribute("aria-pressed", String(forceExp));
    if (forceExp && !ta.value) { ta.value = "Split persistence out of the queue,\nkeep the HTTP behavior identical,\nthen run the browser tests."; } else if (!forceExp && ta.value.startsWith("Split persistence out")) ta.value = "";
    size();
  }
  ta.addEventListener("focus", syncExp); ta.addEventListener("blur", syncExp); ta.addEventListener("input", syncExp);
  let queued = 1;
  ta.addEventListener("keydown", (e) => {
    if (e.isComposing) return;
    if (e.key === "Tab" && !e.shiftKey && ta.value.trim()) { e.preventDefault(); queued++; $$(".seg[data-pop=queue] span:last-child").forEach((s) => (s.textContent = queued)); ta.value = ""; syncExp(); }
    else if (e.key === "Enter" && !e.shiftKey && ta.value.trim()) { e.preventDefault(); say(ta.value.trim()); ta.value = ""; syncExp(); }
  });
  send.addEventListener("click", () => { if (ta.value.trim()) { say(ta.value.trim()); ta.value = ""; syncExp(); } });
  function say(text) {
    const chat = $(".chat"), w = $(".working");
    const u = document.createElement("article"); u.className = "msg user";
    const who = document.createElement("div"); who.className = "who"; who.textContent = "You";
    const body = document.createElement("div"); body.className = "body"; body.textContent = text; u.append(who, body);
    chat.insertBefore(u, $(".plan")); thread.scrollTo({ top: thread.scrollHeight, behavior: "smooth" });
  }

  // Jump to latest: appears when scrolled up; never pulls on its own.
  function jump() { $("#jump").hidden = thread.scrollHeight - thread.scrollTop - thread.clientHeight < 160 || app.dataset.view !== "chat"; }
  thread.addEventListener("scroll", jump);
  new ResizeObserver(() => { root.style.setProperty("--dock-h", $("#dock").offsetHeight + "px"); jump(); }).observe($("#dock"));

  // Files: keyboard-navigable read-only tree with filter.
  const FILES = [
    { n: "src", d: [{ n: "queue-store.ts" }, { n: "prompt-queue.ts" }, { n: "index.ts" }] },
    { n: "tests", d: [{ n: "queue.test.ts" }, { n: "durable-queue.test.ts" }] },
    { n: "README.md" }, { n: "package.json" },
  ];
  const PREVIEW = {
    "prompt-queue.ts": "export class PromptQueue {\n  constructor(runtime, store) {\n    this.runtime = runtime;\n    this.store = store;\n  }\n  enqueue(prompt) { /* ordering only */ }\n}\n",
    "queue-store.ts": "import { readFileSync } from \"node:fs\";\n\nexport function load(file) {\n  const text = readFileSync(file, \"utf8\");\n  return JSON.parse(text);\n}\n",
  };
  const open = new Set(["src"]); let sel = "prompt-queue.ts"; const tree = $("#tree"), filter = $("#filter");
  const mk = (tag, cls, text) => { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; };
  function icon(id, cls) { const s = document.createElementNS("http://www.w3.org/2000/svg", "svg"); s.setAttribute("class", "i " + (cls || "")); const u = document.createElementNS("http://www.w3.org/2000/svg", "use"); u.setAttribute("href", "#" + id); s.append(u); return s; }
  function renderTree() {
    const q = filter.value.trim().toLowerCase(); tree.replaceChildren();
    const walk = (list, parent, level) => list.forEach((it) => {
      const match = !q || it.n.toLowerCase().includes(q) || (it.d && it.d.some((c) => c.n.toLowerCase().includes(q)));
      if (!match) return;
      const li = mk("li"); li.setAttribute("role", "none");
      const b = mk("button", "trow" + (it.n === sel ? " sel" : "")); b.type = "button"; b.setAttribute("role", "treeitem"); b.dataset.name = it.n; b.style.setProperty("--lvl", level);
      b.tabIndex = -1; b.setAttribute("aria-level", level + 1);
      if (it.d) { const ex = q || open.has(it.n); b.setAttribute("aria-expanded", String(!!ex)); b.append(icon("i-chev", "chev"), icon("i-folder"), mk("span", "tn", it.n)); li.append(b); if (ex) { const ul = mk("ul"); ul.setAttribute("role", "group"); li.append(ul); walk(it.d, ul, level + 1); } }
      else { b.append(mk("span", "chev-sp"), icon("i-file"), mk("span", "tn", it.n)); if (it.n === sel) b.setAttribute("aria-selected", "true"); li.append(b); }
      parent.append(li);
    });
    walk(FILES, tree, 0);
    const rows = $$(".trow", tree); (rows.find((r) => r.dataset.name === sel) || rows[0])?.setAttribute("tabindex", "0");
    if (!rows.length) { const li = mk("li", "tree-empty", "No files match “" + filter.value + "”."); tree.append(li); }
  }
  function choose(name) { sel = name; $("#pv-path").textContent = (name.endsWith(".ts") && FILES[0].d.some((c) => c.n === name) ? "src/" : "") + name; $("#pv-code").textContent = PREVIEW[name] || "// " + name + "\n"; $("#pv-meta").textContent = "Read-only preview"; if (innerWidth <= 900) { $("#files").dataset.ftab = "preview"; $$("[data-ftab]").forEach((b) => b.setAttribute("aria-pressed", String(b.dataset.ftab === "preview"))); } renderTree(); $$(".trow", tree).find((r) => r.dataset.name === name)?.focus(); }
  tree.addEventListener("click", (e) => { const b = e.target.closest(".trow"); if (!b) return; const n = b.dataset.name; if (b.hasAttribute("aria-expanded")) { open.has(n) ? open.delete(n) : open.add(n); renderTree(); $$(".trow", tree).find((r) => r.dataset.name === n)?.focus(); } else choose(n); });
  tree.addEventListener("keydown", (e) => {
    const rows = $$(".trow", tree), i = rows.indexOf(document.activeElement); if (i < 0) return;
    const go = (j) => { rows.forEach((r) => (r.tabIndex = -1)); rows[j].tabIndex = 0; rows[j].focus(); };
    if (e.key === "ArrowDown") { e.preventDefault(); go(Math.min(i + 1, rows.length - 1)); }
    else if (e.key === "ArrowUp") { e.preventDefault(); go(Math.max(i - 1, 0)); }
    else if (e.key === "Home") { e.preventDefault(); go(0); } else if (e.key === "End") { e.preventDefault(); go(rows.length - 1); }
    else if (e.key === "ArrowRight" && rows[i].getAttribute("aria-expanded") === "false") { open.add(rows[i].dataset.name); const n = rows[i].dataset.name; renderTree(); $$(".trow", tree).find((r) => r.dataset.name === n)?.focus(); }
    else if (e.key === "ArrowLeft" && rows[i].getAttribute("aria-expanded") === "true") { const n = rows[i].dataset.name; open.delete(n); renderTree(); $$(".trow", tree).find((r) => r.dataset.name === n)?.focus(); }
  });
  filter.addEventListener("input", renderTree);
  renderTree(); $("#pv-code").textContent = PREVIEW[sel];

  // Initial state from the query string (used for screenshots and links).
  const q = new URLSearchParams(location.search);
  if (q.get("view")) setView(q.get("view"));
  if (q.get("panel")) setPanel(q.get("panel"), true);
  if (q.get("pop")) pop(q.get("pop"));
  if (q.get("menu")) setMenu(true);
  if (q.get("composer") === "expanded") { forceExp = true; syncExp(); }
  if (q.get("pv") === "0") { $("#pv").open = false; document.body.dataset.pv = "off"; }
  setTimeout(() => { thread.scrollTop = thread.scrollHeight; jump(); }, 30);
  if (q.get("scroll") === "up") setTimeout(() => { thread.scrollTop = 120; jump(); }, 60);
})();
