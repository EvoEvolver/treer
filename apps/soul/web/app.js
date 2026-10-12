const elements = {
  count: document.querySelector("#count"),
  detail: document.querySelector("#detail"),
  list: document.querySelector("#list"),
  notice: document.querySelector("#notice"),
  refresh: document.querySelector("#refresh"),
  summary: document.querySelector("#summary"),
};

const state = {
  selectedId: null,
  principal: null,
  souls: [],
};

function applicationUrl(path) {
  return new URL(path.replace(/^\/+/, ""), window.location.href).toString();
}

function formatBytes(value) {
  if (!Number.isFinite(value) || value < 0) return "Unknown";
  if (value < 1024) return `${value} B`;
  const units = ["KB", "MB", "GB"];
  let amount = value / 1024;
  let unit = units[0];
  for (let index = 1; amount >= 1024 && index < units.length; index += 1) {
    amount /= 1024;
    unit = units[index];
  }
  return `${amount.toFixed(amount >= 10 ? 1 : 2)} ${unit}`;
}

function formatDate(value) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "Unknown";
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date);
}

function showError(error) {
  elements.notice.textContent = error instanceof Error ? error.message : String(error);
  elements.notice.classList.add("visible");
}

function clearError() {
  elements.notice.textContent = "";
  elements.notice.classList.remove("visible");
}

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function adapterName(soul) {
  return soul.manifest?.adapter?.name === "codex" ? "Codex" : "Generic";
}

function renderList() {
  elements.list.replaceChildren();
  elements.count.textContent = String(state.souls.length);
  elements.summary.textContent = `${state.souls.length} ${state.souls.length === 1 ? "soul" : "souls"}`;

  if (state.souls.length === 0) {
    elements.list.append(element("p", "list-empty", "No souls stored"));
    return;
  }

  for (const soul of state.souls) {
    const row = element("button", "soul-row");
    row.type = "button";
    row.classList.toggle("selected", soul.soul_id === state.selectedId);
    row.append(
      element("strong", "", soul.name || soul.manifest?.name || "Soul"),
      element("span", "", `v${soul.version} · ${soul.harness} · ${formatBytes(soul.size_bytes)} · ${soul.owner} · ${formatDate(soul.created_at)}${soul.contains_secrets ? " · Contains secrets" : ""}`),
    );
    row.addEventListener("click", () => {
      state.selectedId = soul.soul_id;
      renderList();
      renderDetail(soul);
    });
    elements.list.append(row);
  }
}

function addFact(container, label, value) {
  const fact = element("div", "fact");
  const term = element("dt", "", label);
  const description = element("dd", "", value);
  fact.append(term, description);
  container.append(fact);
}

function renderBindings(container, environment) {
  const section = element("section", "block");
  section.append(element("h3", "", "Environment"));
  const entries = Object.entries(environment || {});
  if (entries.length === 0) {
    section.append(element("p", "muted", "No environment bindings"));
  } else {
    const list = element("dl", "bindings");
    for (const [name, path] of entries) {
      list.append(element("dt", "", name), element("dd", "", String(path)));
    }
    section.append(list);
  }
  container.append(section);
}

function renderFiles(container, files) {
  const section = element("section", "block");
  section.append(element("h3", "", "Files"));
  if (!Array.isArray(files) || files.length === 0) {
    section.append(element("p", "muted", "No files"));
    container.append(section);
    return;
  }

  const table = element("table", "file-table");
  const head = document.createElement("thead");
  const headRow = document.createElement("tr");
  headRow.append(element("th", "", "Path"), element("th", "", "Size"));
  head.append(headRow);
  const body = document.createElement("tbody");
  for (const file of files) {
    const row = document.createElement("tr");
    row.append(
      element("td", "", String(file.path || "")),
      element("td", "", formatBytes(Number(file.size))),
    );
    body.append(row);
  }
  table.append(head, body);
  section.append(table);
  container.append(section);
}

function renderDetail(soul) {
  const fragment = document.createDocumentFragment();
  const header = element("header", "detail-header");
  const titleRow = element("div", "detail-title-row");
  titleRow.append(
    element("h2", "", soul.name || soul.manifest?.name || "Soul"),
    element("span", "badge", adapterName(soul)),
  );
  header.append(titleRow, element("p", "soul-id", soul.soul_id));

  const facts = element("dl", "facts");
  addFact(facts, "Owner", soul.owner);
  addFact(facts, "Version", String(soul.version));
  addFact(facts, "Visibility", soul.visibility);
  addFact(facts, "Secrets", soul.contains_secrets ? "Confirmed secrets present" : "None detected");
  addFact(facts, "Created", formatDate(soul.created_at));
  addFact(facts, "Archive", formatBytes(Number(soul.archive_size)));
  addFact(facts, "Files", String(Array.isArray(soul.files) ? soul.files.length : 0));
  header.append(facts);
  fragment.append(header);

  renderBindings(fragment, soul.manifest?.environment);
  renderFiles(fragment, soul.files);

  const integrity = element("section", "block");
  integrity.append(
    element("h3", "", "SHA-256"),
    element("p", "hash", soul.archive_sha256 || "Unknown"),
  );
  fragment.append(integrity);
  const manifestBlock = element("section", "block");
  manifestBlock.append(element("h3", "", "Manifest"), element("pre", "manifest", JSON.stringify(soul.manifest, null, 2)));
  fragment.append(manifestBlock);
  const command = `treer-soul incarnate ${soul.soul_id} --machine MACHINE`;
  const commands = element("section", "block");
  commands.append(element("h3", "", "Incarnate"), element("pre", "manifest", command));
  const copy = element("button", "", "Copy command");
  copy.addEventListener("click", async () => {
    try { await navigator.clipboard.writeText(command); copy.textContent = "Copied"; }
    catch { showError(new Error("Copy the command shown above")); }
  });
  commands.append(copy);
  if (soul.visibility === "private") commands.append(element("p", "muted", "A new Agent needs explicit reader access or a workspace-visible snapshot."));
  if (soul.owner === state.principal?.id) {
    const remove = element("button", "", "Delete soul");
    remove.addEventListener("click", async () => {
      if (!window.confirm("Permanently clean up this soul's files?")) return;
      try { await api(`v1/souls/${soul.soul_id}`, {method: "DELETE", headers: {"X-Soul-Request": "1"}}); await load(); }
      catch (error) { showError(error); }
    });
    commands.append(remove);
  }
  fragment.append(commands);
  elements.detail.replaceChildren(fragment);
}

async function api(path, options = {}) {
  const response = await fetch(applicationUrl(path), options);
  const body = await response.json();
  if (!response.ok) {
    const error = new Error(body.error?.message || `Request failed (${response.status})`);
    error.status = response.status; error.code = body.error?.code;
    throw error;
  }
  return body;
}

async function load() {
  clearError();
  elements.refresh.disabled = true;
  elements.summary.textContent = "Loading";
  try {
    state.principal = (await api("v1/me")).principal;
    document.querySelector("#login").hidden = true;
    const all = [];
    let offset = 0;
    let body;
    do {
      body = await api(`v1/souls?limit=100&offset=${offset}`);
      all.push(...body.souls);
      offset = body.next_offset;
    } while (offset !== null);
    body.souls = all;
    state.souls = Array.isArray(body.souls) ? body.souls : [];
    if (!state.souls.some((soul) => soul.soul_id === state.selectedId)) {
      state.selectedId = state.souls[0]?.soul_id || null;
    }
    renderList();
    elements.summary.textContent += ` · My usage: ${body.usage.count} souls / ${formatBytes(body.usage.bytes)}`;
    const selected = state.souls.find((soul) => soul.soul_id === state.selectedId);
    if (selected) renderDetail(selected);
    else elements.detail.replaceChildren(element("p", "muted", "No soul selected"));
  } catch (error) {
    state.souls = [];
    renderList();
    document.querySelector("#login").hidden = error.status !== 401 || error.code !== "authentication_required";
    showError(error);
    elements.summary.textContent = "Unavailable";
  } finally {
    elements.refresh.disabled = false;
  }
}

elements.refresh.addEventListener("click", load);
load();
