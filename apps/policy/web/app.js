const state = { bundle: null, mode: "monitor", dirty: false }

function applicationUrl(path) {
  const base = new URL("./", window.location.href)
  return new URL(path.replace(/^\//, ""), base).toString()
}

async function request(path, options = {}) {
  const response = await fetch(applicationUrl(path), {
    ...options,
    headers: { "content-type": "application/json", ...(options.headers || {}) },
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
    node.innerHTML = `<span class="decision-mark">${result.decision === "allow" ? "A" : "D"}</span><div><small>Decision · revision ${result.revision}</small><strong>${result.decision}</strong><p>${result.matched_rule_id ? `Rule ${result.matched_rule_id} · computed ${result.effect}` : `Default · computed ${result.effect}`}${result.mode === "monitor" && result.effect === "deny" ? " · monitor mode" : ""}</p></div>`
  } catch (error) { showError("test-error", error) }
})

load()
