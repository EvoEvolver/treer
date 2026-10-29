"use strict";
(() => {
  const $ = (id) => document.getElementById(id);
  const headers = { "X-Gateway-Request": "1" };

  async function call(method, path, body) {
    const init = { method, headers: { ...headers }, credentials: "same-origin" };
    if (body !== undefined) {
      init.headers["Content-Type"] = "application/json";
      init.body = JSON.stringify(body);
    }
    const response = await fetch(path, init);
    const value = await response.json().catch(() => ({}));
    if (!response.ok) throw new Error((value.error && value.error.message) || `HTTP ${response.status}`);
    return value;
  }

  function notice(text, error) {
    const node = $("notice");
    node.hidden = !text;
    node.textContent = text || "";
    node.className = error ? "notice error" : "notice";
  }

  function el(tag, text, className) {
    const node = document.createElement(tag);
    if (text !== undefined && text !== null) node.textContent = String(text);
    if (className) node.className = className;
    return node;
  }

  function pairs(target, rows) {
    target.replaceChildren();
    for (const [label, value, className] of rows) {
      target.append(el("dt", label), el("dd", value === null || value === undefined || value === "" ? "—" : value, className));
    }
  }

  function when(seconds) {
    return seconds ? new Date(seconds * 1000).toLocaleString() : null;
  }

  async function loadStatus() {
    const status = await call("GET", "api/status");
    const credential = status.credential;
    const rate = status.rate_limit || {};
    if (status.caller) $("whoami").textContent = `${status.caller.kind}: ${status.caller.name}`;
    pairs($("status"), [
      ["Active credential", credential.active || `none — ${credential.error || "unavailable"}`, credential.active ? "ok" : "error"],
      ["Credential mode", credential.mode],
      ["Agent URL", status.agent_url],
      ["Clone example", status.repos && status.repos[0] ? `git clone ${status.agent_url}/github/${status.repos[0].replace("*", "REPO")}.git` : null],
      ["Allowed repos", (status.repos || []).join(", ")],
      ["Rate limit", rate.remaining !== undefined && rate.remaining !== null ? `${rate.remaining} / ${rate.limit} (resets ${when(rate.reset)})` : null],
    ]);
    const gh = credential.gh;
    const ghBox = $("gh-state");
    ghBox.replaceChildren(el("h3", "gh mode (default, no setup)"));
    const ghList = el("dl", null, "grid");
    pairs(ghList, [
      ["gh login", gh.available ? `detected (${gh.token_type || "token"}) on ${gh.hostname}` : gh.error || "not read yet", gh.available ? "ok" : "warn"],
      ["Last read", gh.fetched_at],
      ["Next re-read", gh.next_refresh_at],
    ]);
    ghBox.append(ghList);
    const app = credential.github_app;
    const appBox = $("app-state");
    appBox.replaceChildren();
    if (app.configured) {
      appBox.append(el("h3", "GitHub App"));
      const list = el("dl", null, "grid");
      pairs(list, [
        ["App", `${app.app.name || app.app.slug} (id ${app.app.id})`],
        ["Owner", app.app.owner],
        ["Installations", app.installations.length ? app.installations.map((i) => `${i.account} (${i.repository_selection})`).join(", ") : "not installed yet", app.installations.length ? "ok" : "warn"],
        ["Token expires", app.token_expires_at],
        ["Error", app.error],
      ]);
      appBox.append(list);
      if (app.install_url) {
        const link = el("a", "Install the GitHub App on repositories");
        link.href = app.install_url;
        link.target = "_blank";
        link.rel = "noopener noreferrer";
        appBox.append(el("p")).append(link);
      }
    }
    $("create-app").hidden = app.configured;
    $("mode").value = credential.mode;
  }

  async function loadConfig() {
    const value = await call("GET", "api/config");
    $("config").value = JSON.stringify(value.config, null, 2);
  }

  async function loadAudit() {
    const query = new URLSearchParams({ limit: "200" });
    for (const [key, id] of [["repo", "filter-repo"], ["op", "filter-op"], ["result", "filter-result"]]) {
      if ($(id).value) query.set(key, $(id).value.trim());
    }
    const value = await call("GET", `api/audit?${query}`);
    const body = $("audit");
    body.replaceChildren();
    for (const entry of value.entries) {
      const row = el("tr");
      const caller = entry.caller || {};
      const refs = (entry.refs || []).map((r) => `${r.ref} ${r.old.slice(0, 7)}→${r.new.slice(0, 7)} ${r.result || ""}`).join("\n");
      const target = entry.op === "api" ? `${entry.method} ${entry.path}` : entry.repo;
      const result = `${entry.result}${entry.status ? ` ${entry.status}` : ""}${entry.reason ? ` — ${entry.reason}` : ""}`;
      row.append(
        el("td", new Date(entry.at).toLocaleString()),
        el("td", `${caller.kind || ""} ${caller.name || caller.id || ""}${caller.machine_id ? ` @${caller.machine_id}` : ""}`),
        el("td", entry.op),
        el("td", target),
        el("td", refs),
        el("td", result, entry.result),
      );
      row.children[4].style.whiteSpace = "pre-line";
      body.append(row);
    }
    if (!value.entries.length) {
      const row = el("tr");
      const cell = el("td", "No entries", "muted");
      cell.colSpan = 6;
      row.append(cell);
      body.append(row);
    }
  }

  async function guarded(action, success) {
    try {
      await action();
      if (success) notice(success);
    } catch (error) {
      notice(error.message, true);
    }
  }

  function refreshAll() {
    return guarded(async () => { await Promise.all([loadStatus(), loadConfig(), loadAudit()]); });
  }

  $("refresh").addEventListener("click", refreshAll);
  $("reload-audit").addEventListener("click", () => guarded(loadAudit));
  $("save-config").addEventListener("click", () => guarded(async () => {
    let value;
    try { value = JSON.parse($("config").value); } catch { throw new Error("Rules are not valid JSON"); }
    const saved = await call("PUT", "api/config", value);
    $("config").value = JSON.stringify(saved.config, null, 2);
    await loadStatus();
  }, "Rules saved"));
  $("save-mode").addEventListener("click", () => guarded(async () => {
    await call("PUT", "api/config", { credential_mode: $("mode").value });
    await Promise.all([loadStatus(), loadConfig()]);
  }, "Credential mode saved"));
  $("reload-credentials").addEventListener("click", () => guarded(async () => {
    await call("POST", "api/credentials/refresh", {});
    await loadStatus();
  }, "Credentials re-read"));
  $("prepare-app").addEventListener("click", () => guarded(async () => {
    const org = $("org").value.trim();
    const value = await call("POST", "api/github-app/manifest", org ? { org } : {});
    const form = $("manifest-form");
    form.action = value.post_url;
    $("manifest").value = value.manifest;
    form.hidden = false;
  }, "Manifest ready: continue on GitHub, then return here and click Refresh"));

  const params = new URLSearchParams(location.search);
  if (params.get("github_app") === "created") notice("GitHub App created. Install it, then click Refresh.");
  if (params.get("github_app") === "installed") notice("GitHub App installed. Click Refresh to see installations.");
  refreshAll();
})();
