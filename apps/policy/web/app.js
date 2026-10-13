const state = { bundle: null, mode: "monitor", dirty: false }

function applicationUrl(path) {
  const base = new URL("./", window.location.href)
  return new URL(path.replace(/^\//, ""), base).toString()
}

async function request(path, options = {}) {
  const response = await fetch(applicationUrl(path), {
    ...options,
    headers: { "content-type": "application/json", ...((["v1/policy/publish","v1/policy/invalidate","v1/policy/rollback","v1/policy/history"].includes(path) || path.startsWith("v1/policy/revisions/")) ? {Authorization: "Bearer " + document.getElementById("admin-token").value} : {}), ...(options.headers || {}) },
  })
  const body = await response.json()
  if (!response.ok) throw new Error(body.error?.message || `HTTP ${response.status}`)
  return body
}

function text(id, value) { document.getElementById(id).textContent = String(value) }
function showError(id, error) { const node = document.getElementById(id); node.textContent = error?.message || String(error); node.classList.remove("hidden") }
function clearError(id) { document.getElementById(id).classList.add("hidden") }
function toast(message) { const node = document.getElementById("toast"); node.textContent = message; node.classList.remove("hidden"); window.setTimeout(() => node.classList.add("hidden"), 2600) }

function render(status) {
  state.bundle = status.bundle
  state.mode = status.bundle.mode
  state.dirty = false
  text("workspace", status.bundle.workspace_id)
  text("header-revision", `rev ${status.bundle.revision}`)
  text("metric-mode", status.bundle.mode)
  text("metric-revision", status.bundle.revision)
  text("metric-rules", status.bundle.document.rules.length)
  text("metric-sync", status.proxy_sync.status.replace("_", " "))
  text("published-at", new Date(status.bundle.generated_at).toLocaleString())
  text("editor-state", `Current revision ${status.bundle.revision}`)
  document.getElementById("policy-editor").value = JSON.stringify(status.bundle.document, null, 2)
  document.querySelectorAll("[data-mode]").forEach((button) => button.classList.toggle("active", button.dataset.mode === state.mode))
  document.getElementById("status-dot").classList.add("ok")
  text("server-status", "Serving bundle")
  const syncError = document.getElementById("sync-error")
  if (status.proxy_sync.status === "failed" || status.proxy_sync.status === "unavailable") {
    syncError.textContent = status.proxy_sync.message
    syncError.classList.remove("hidden")
  } else syncError.classList.add("hidden")
}

async function load() {
  try { render(await request("v1/state")) }
  catch (error) { showError("sync-error", error); text("server-status", "Unavailable") }
}

document.querySelectorAll(".nav-item").forEach((button) => button.addEventListener("click", () => {
  document.querySelectorAll(".nav-item").forEach((item) => item.classList.toggle("active", item === button))
  document.querySelectorAll(".view").forEach((view) => view.classList.toggle("active", view.id === `view-${button.dataset.view}`))
  if (button.dataset.view === "history") void loadHistory()
}))

document.querySelectorAll("[data-mode]").forEach((button) => button.addEventListener("click", () => {
  state.mode = button.dataset.mode
  state.dirty = true
  document.querySelectorAll("[data-mode]").forEach((item) => item.classList.toggle("active", item === button))
  text("editor-state", "Unpublished changes")
}))

document.getElementById("policy-editor").addEventListener("input", () => { state.dirty = true; text("editor-state", "Unpublished changes") })
document.getElementById("policy-editor").addEventListener("keydown", (event) => {
  if (event.key === "Tab") {
    event.preventDefault()
    const editor = event.currentTarget
    editor.setRangeText("  ", editor.selectionStart, editor.selectionEnd, "end")
    editor.dispatchEvent(new Event("input"))
  }
})

document.getElementById("format-button").addEventListener("click", () => {
  clearError("editor-error")
  try {
    const editor = document.getElementById("policy-editor")
    editor.value = JSON.stringify(JSON.parse(editor.value), null, 2)
  } catch (error) { showError("editor-error", error) }
})

document.getElementById("publish-button").addEventListener("click", async () => {
  clearError("editor-error")
  const button = document.getElementById("publish-button")
  button.disabled = true
  try {
    const body = await request("v1/policy/publish", { method: "POST", body: JSON.stringify({ expected_revision: state.bundle.revision, mode: state.mode, document: JSON.parse(document.getElementById("policy-editor").value) }) })
    render({ bundle: body.bundle, proxy_sync: body.proxy_sync })
    toast(body.proxy_sync.status === "synced" ? "Published and Proxy cache invalidated" : "Published; Proxy sync needs attention")
  } catch (error) { showError("editor-error", error) }
  finally { button.disabled = false }
})

document.getElementById("sync-button").addEventListener("click", async () => {
  clearError("sync-error")
  try {
    const result = await request("v1/policy/invalidate", { method: "POST", body: "{}" })
    const status = await request("v1/state")
    render(status)
    toast(result.proxy_sync.status === "synced" ? "Proxy cache invalidated" : "Sync failed")
  } catch (error) { showError("sync-error", error) }
})

document.getElementById("test-button").addEventListener("click", async () => {
  clearError("test-error")
  try {
    const result = await request("v1/policy/test", { method: "POST", body: JSON.stringify({
      subject: { kind: document.getElementById("subject-kind").value, id: document.getElementById("subject-id").value, machine_id: document.getElementById("machine-id").value },
      action: document.getElementById("action").value,
      resource: { kind: document.getElementById("resource-kind").value, id: document.getElementById("resource-id").value },
    }) })
    const node = document.getElementById("decision")
    node.className = `decision ${result.decision}`
    const marker = document.createElement("span");marker.className="decision-mark";marker.textContent=result.decision === "allow" ? "A" : "D";
    const details = document.createElement("div"), label = document.createElement("small"), decision = document.createElement("strong"), explanation = document.createElement("p");
    label.textContent=`Decision · revision ${result.revision}`;decision.textContent=result.decision;
    explanation.textContent=result.matched_rule_id ? `Rule ${result.matched_rule_id} · computed ${result.effect}` : `Default · computed ${result.effect}`;
    if(result.mode === "monitor" && result.effect === "deny") explanation.textContent += " · monitor mode";
    details.append(label,decision,explanation);node.replaceChildren(marker,details);
  } catch (error) { showError("test-error", error) }
})

load()

async function loadHistory() {
  clearError("history-error")
  try {
    const result = await request("v1/policy/history")
    const list = document.getElementById("history-list")
    list.replaceChildren()
    for (const revision of result.revisions) {
      const row = document.createElement("article"), label = document.createElement("p")
      label.textContent = `Revision ${revision.revision} · ${revision.mode} · ${revision.published_at}` + (revision.restored_from ? ` · restored from ${revision.restored_from}` : "")
      const inspect = document.createElement("button"), rollback = document.createElement("button")
      inspect.textContent = "View document"; inspect.className = "secondary"
      inspect.addEventListener("click", async () => {
        try { document.getElementById("history-document").textContent = JSON.stringify(await request(`v1/policy/revisions/${revision.revision}`), null, 2) }
        catch (error) { showError("history-error", error) }
      })
      rollback.textContent = `Restore revision ${revision.revision}`
      rollback.disabled = revision.revision === state.bundle.revision
      rollback.addEventListener("click", async () => {
        if (!window.confirm(`Restore revision ${revision.revision} (${revision.mode}) as a new policy revision? This changes active permissions.`)) return
        rollback.disabled = true
        try {
          render(await request("v1/policy/rollback", {method:"POST", body:JSON.stringify({expected_revision:state.bundle.revision, revision:revision.revision})}))
          await loadHistory();toast("Historical policy restored as a new revision")
        } catch (error) { showError("history-error", error);rollback.disabled = false }
      })
      row.append(label,inspect,rollback);list.append(row)
    }
  } catch (error) { showError("history-error", error) }
}
document.getElementById("history-refresh").addEventListener("click",loadHistory)
