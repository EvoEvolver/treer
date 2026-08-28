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
  souls: [],
};

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
      element("strong", "", soul.manifest?.name || "Soul"),
      element("span", "", `${adapterName(soul)} · ${formatDate(soul.created_at)}`),
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
    element("h2", "", soul.manifest?.name || "Soul"),
    element("span", "badge", adapterName(soul)),
  );
  header.append(titleRow, element("p", "soul-id", soul.soul_id));

  const facts = element("dl", "facts");
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
  elements.detail.replaceChildren(fragment);
}

async function load() {
  clearError();
  elements.refresh.disabled = true;
  elements.summary.textContent = "Loading";
  try {
    const response = await fetch("v1/souls", { headers: { Accept: "application/json" } });
    const body = await response.json();
    if (!response.ok) throw new Error(body.error?.message || `Request failed (${response.status})`);
    state.souls = Array.isArray(body.souls) ? body.souls : [];
    if (!state.souls.some((soul) => soul.soul_id === state.selectedId)) {
      state.selectedId = state.souls[0]?.soul_id || null;
    }
    renderList();
    const selected = state.souls.find((soul) => soul.soul_id === state.selectedId);
    if (selected) renderDetail(selected);
  } catch (error) {
    state.souls = [];
    renderList();
    showError(error);
    elements.summary.textContent = "Unavailable";
  } finally {
    elements.refresh.disabled = false;
  }
}

elements.refresh.addEventListener("click", load);
load();
