const elements = {
  cancelDialog: document.querySelector("#cancel-dialog"),
  closeDialog: document.querySelector("#close-dialog"),
  count: document.querySelector("#count"),
  createDialog: document.querySelector("#create-dialog"),
  createForm: document.querySelector("#create-form"),
  detail: document.querySelector("#detail"),
  newRepo: document.querySelector("#new-repo"),
  notice: document.querySelector("#notice"),
  refresh: document.querySelector("#refresh"),
  repos: document.querySelector("#repos"),
};

const state = { repos: [], selected: null };

function node(tag, className, text) {
  const result = document.createElement(tag);
  if (className) result.className = className;
  if (text !== undefined) result.textContent = text;
  return result;
}

function formatDate(value) {
  const date = new Date(value);
  return Number.isNaN(date.getTime())
    ? "Unknown"
    : new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(date);
}

function showError(error) {
  elements.notice.textContent = error instanceof Error ? error.message : String(error);
  elements.notice.classList.add("visible");
}

function clearError() {
  elements.notice.textContent = "";
  elements.notice.classList.remove("visible");
}

function applicationUrl(path) {
  return new URL(path.replace(/^\/+/, ""), window.location.href).toString();
}

async function json(path, init) {
  const response = await fetch(applicationUrl(path), {
    ...init,
    headers: { Accept: "application/json", ...init?.headers },
  });
  const body = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(body.error?.message || `Request failed (${response.status})`);
  return body;
}

function renderList() {
  elements.repos.replaceChildren();
  elements.count.textContent = String(state.repos.length);
  if (!state.repos.length) {
    elements.repos.append(node("p", "list-empty", "No repositories"));
    return;
  }
  for (const repo of state.repos) {
    const button = node("button", "repo-row");
    button.type = "button";
    button.classList.toggle("selected", repo.name === state.selected);
    button.append(
      node("strong", "", repo.name),
      node("span", "", repo.description || "No description"),
      node("small", "", `${repo.branch_count} ${repo.branch_count === 1 ? "branch" : "branches"}`),
    );
    button.addEventListener("click", () => selectRepository(repo.name));
    elements.repos.append(button);
  }
}

function renderDetail(repo) {
  const fragment = document.createDocumentFragment();
  const header = node("header", "repo-header");
  const title = node("div", "title-row");
  title.append(node("h2", "", repo.name), node("span", "visibility", "Internal"));
  header.append(title, node("p", "description", repo.description || "No description"));

  const clone = node("div", "clone-row");
  const cloneUrl = node("code", "", repo.clone_url);
  const copy = node("button", "", "Copy");
  copy.type = "button";
  copy.addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(repo.clone_url);
      copy.textContent = "Copied";
      setTimeout(() => { copy.textContent = "Copy"; }, 1200);
    } catch (error) { showError(error); }
  });
  clone.append(cloneUrl, copy);
  header.append(clone);

  const facts = node("dl", "facts");
  for (const [label, value] of [
    ["Default branch", repo.default_branch],
    ["Branches", String(repo.branch_count)],
    ["Updated", formatDate(repo.updated_at)],
  ]) {
    const item = node("div", "fact");
    item.append(node("dt", "", label), node("dd", "", value));
    facts.append(item);
  }
  header.append(facts);
  fragment.append(header);

  const branches = node("section", "block");
  branches.append(node("h3", "", "Branches"));
  if (!repo.branches.length) branches.append(node("p", "muted", "No branches yet"));
  else {
    const list = node("div", "rows");
    for (const branch of repo.branches) {
      const row = node("div", "data-row");
      row.append(node("strong", "", branch.name), node("code", "", branch.commit.slice(0, 10)), node("time", "", formatDate(branch.committed_at)));
      list.append(row);
    }
    branches.append(list);
  }
  fragment.append(branches);

  const commits = node("section", "block");
  commits.append(node("h3", "", "Recent commits"));
  if (!repo.recent_commits.length) commits.append(node("p", "muted", "Push the first commit to begin"));
  else {
    const list = node("div", "rows");
    for (const commit of repo.recent_commits) {
      const row = node("div", "data-row commit-row");
      const summary = node("span", "commit-summary");
      summary.append(node("strong", "", commit.subject), node("small", "", `${commit.author} / ${formatDate(commit.committed_at)}`));
      row.append(summary, node("code", "", commit.short_commit));
      list.append(row);
    }
    commits.append(list);
  }
  fragment.append(commits);
  elements.detail.replaceChildren(fragment);
}

async function selectRepository(name) {
  clearError();
  try {
    const body = await json(`/v1/repos/${encodeURIComponent(name)}`);
    state.selected = name;
    renderList();
    renderDetail(body.repo);
  } catch (error) { showError(error); }
}

async function load() {
  clearError();
  elements.refresh.disabled = true;
  try {
    const body = await json("/v1/repos");
    state.repos = Array.isArray(body.repos) ? body.repos : [];
    if (!state.repos.some((repo) => repo.name === state.selected)) state.selected = state.repos[0]?.name || null;
    renderList();
    if (state.selected) await selectRepository(state.selected);
  } catch (error) { showError(error); }
  finally { elements.refresh.disabled = false; }
}

elements.createForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  clearError();
  const data = new FormData(elements.createForm);
  try {
    const body = await json("/v1/repos", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name: data.get("name"), description: data.get("description") }),
    });
    elements.createDialog.close();
    elements.createForm.reset();
    state.selected = body.repo.name;
    await load();
  } catch (error) { showError(error); }
});

elements.newRepo.addEventListener("click", () => elements.createDialog.showModal());
elements.closeDialog.addEventListener("click", () => elements.createDialog.close());
elements.cancelDialog.addEventListener("click", () => elements.createDialog.close());
elements.refresh.addEventListener("click", load);
load();
