const state = { bundle: null, mode: "monitor", dirty: false, editing: null, decisions: [] }

function applicationUrl(path) {
  const base = new URL("./", window.location.href)
  return new URL(path.replace(/^\//, ""), base).toString()
}

async function request(path, options = {}) {
  const response = await fetch(applicationUrl(path), {
    ...options,
    headers: { "content-type": "application/json", "X-Policy-Request":"1",
      ...(document.getElementById("admin-token").value ? {"X-Policy-Admin-Token":document.getElementById("admin-token").value} : {}), ...(options.headers || {}) },
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
  renderRules()
  const syncError = document.getElementById("sync-error")
  if (status.proxy_sync.status === "failed" || status.proxy_sync.status === "unavailable") {
    syncError.textContent = status.proxy_sync.message
    syncError.classList.remove("hidden")
  } else syncError.classList.add("hidden")
}

async function load() {
  try { render(await request("v1/state")); await loadAuthority() }
  catch (error) { showError("sync-error", error); text("server-status", "Unavailable") }
}

document.querySelectorAll(".nav-item").forEach((button) => button.addEventListener("click", () => {
  document.querySelectorAll(".nav-item").forEach((item) => item.classList.toggle("active", item === button))
  document.querySelectorAll(".view").forEach((view) => view.classList.toggle("active", view.id === `view-${button.dataset.view}`))
  if (button.dataset.view === "history") void loadHistory()
  if (button.dataset.view === "templates") void loadTemplates()
  if (button.dataset.view === "logs") void loadLogs()
}))

document.querySelectorAll("[data-mode]").forEach((button) => button.addEventListener("click", () => {
  state.mode = button.dataset.mode
  state.dirty = true
  document.querySelectorAll("[data-mode]").forEach((item) => item.classList.toggle("active", item === button))
  text("editor-state", "Unpublished changes")
}))

document.getElementById("policy-editor").addEventListener("input", () => { state.dirty = true; text("editor-state", "Unpublished changes"); renderRules() })
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
    await validateDraft()
    if (state.mode === "enforce" && !window.confirm("Enforce these rules? They may deny your own non-recovery operations. Review warnings and simulations first.")) return
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
    const body = {
      subject: { kind: value("subject-kind"), id: value("subject-id") },
      action: value("action"), resource: {kind:value("resource-kind"), id:value("resource-id")},
    }
    if(value("actor-path") || value("target-path")) {
      if(value("actor-path")) {delete body.subject.id;body.subject.path=value("actor-path")}
      if(value("target-path")) {delete body.resource.id;body.resource.path=value("target-path")}
    }
    if(document.getElementById("test-draft").checked) {body.document=draft();body.mode=state.mode}
    const response = await request("v1/policy/explain", {method:"POST", body:JSON.stringify(body)})
    const detail = response.explanation
    const result = {decision:detail.effective_decision, effect:detail.computed_effect, mode:detail.mode,
      revision:detail.policy_revision, matched_rule_id:detail.matched_rule_id}

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
loadIdentity()

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
      rollback.textContent = `Restore revision ${revision.revision}`; rollback.className="secondary"
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

function value(id) { return document.getElementById(id).value.trim() }
function draft() { return JSON.parse(document.getElementById("policy-editor").value) }
function setDraft(documentValue) {
  document.getElementById("policy-editor").value = JSON.stringify(documentValue,null,2)
  state.dirty = true; text("editor-state","Unpublished changes");renderRules()
}
function warnings(values) {
  const node = document.getElementById("safety-warnings")
  node.replaceChildren()
  for (const item of values) { const p = document.createElement("p");p.textContent=item;node.append(p) }
  node.classList.toggle("hidden",!values.length)
}
async function validateDraft() {
  const result = await request("v1/policy/validate",{method:"POST",body:JSON.stringify({document:draft()})})
  warnings(result.warnings || []);return result
}
document.getElementById("validate-button").addEventListener("click",async()=>{
  clearError("editor-error")
  try { await validateDraft();toast("Draft validated") } catch(error) {showError("editor-error",error)}
})
async function loadIdentity() {
  try {
    const result = await request("v1/session")
    text("identity-status", result.principal ? `Treer: ${result.principal.name || result.principal.id}` : "No Treer human session")
    document.getElementById("login-link").hidden = !!result.principal || !result.oauth_available
    document.getElementById("logout-button").hidden = !result.principal
  } catch(error) {text("identity-status",error.message);document.getElementById("login-link").hidden=true}
}
document.getElementById("logout-button").addEventListener("click",async()=>{
  try {await request("api/auth/logout",{method:"POST",body:"{}"});await loadIdentity()}
  catch(error) {text("identity-status",error.message)}
})
function renderRules() {
  const list = document.getElementById("rule-list");list.replaceChildren()
  let rules
  try {rules = draft().rules || []} catch {return}
  for (const rule of rules) {
    const card=document.createElement("article"),title=document.createElement("h3"),summary=document.createElement("p")
    title.textContent=`${rule.id} · ${rule.effect} · priority ${rule.priority}`
    summary.textContent=JSON.stringify({subjects:rule.subjects,actions:rule.actions,resources:rule.resources,conditions:rule.conditions})
    const edit=document.createElement("button"),remove=document.createElement("button")
    edit.textContent="Edit rule";edit.className="secondary";edit.addEventListener("click",()=>editRule(rule))
    remove.textContent="Remove from draft";remove.className="secondary";remove.addEventListener("click",()=>{
      const documentValue=draft();documentValue.rules=documentValue.rules.filter(r=>r.id!==rule.id);setDraft(documentValue)
    })
    card.append(title,summary,edit,remove);list.append(card)
  }
}
function fill(id, val) {document.getElementById(id).value=val ?? ""}
function editRule(rule) {
  state.editing=rule.id
  for(const [id,val] of Object.entries({"rule-id":rule.id,"rule-priority":rule.priority,"rule-effect":rule.effect,"rule-actions":rule.actions.join(",")})) fill(id,val)
  for(const [prefix,selector] of [["actor",rule.subjects[0]],["target",rule.resources[0]]]) {
    fill(`rule-${prefix}-kind`,selector.kind)
    fill(`rule-${prefix}-tree-kind`,selector.tree?.kind)
    const match=["path_glob","path_prefix","path_eq"].find(k=>selector.tree?.[k]!==undefined) || ""
    fill(`rule-${prefix}-match`,match);fill(`rule-${prefix}-path`,match==="path_prefix"?selector.tree[match].path:selector.tree?.[match])
  }
  fill("rule-relation","keep")
  toast("Editing first selectors; advanced selectors, bindings and conditions are preserved")
}
document.getElementById("new-rule").addEventListener("click",()=>{
  state.editing=null;document.getElementById("rule-form").reset()
})
document.getElementById("rule-form").addEventListener("submit",event=>{
  event.preventDefault();clearError("editor-error")
  try {
    const documentValue=draft(), old=documentValue.rules.find(r=>r.id===state.editing)
    const rule=old?structuredClone(old):{subjects:[{}],resources:[{}]}
    Object.assign(rule,{id:value("rule-id"),priority:Number(value("rule-priority")),effect:value("rule-effect"),actions:value("rule-actions").split(",").map(s=>s.trim()).filter(Boolean)})
    if(documentValue.rules.some(r=>r.id===rule.id && r.id!==state.editing)) throw new Error("Rule ID already exists")
    for(const [prefix,selectors] of [["actor",rule.subjects],["target",rule.resources]]) {
      const selector=selectors[0],kind=value(`rule-${prefix}-kind`),tree={...selector.tree},match=value(`rule-${prefix}-match`)
      if(kind) selector.kind=kind;else delete selector.kind
      const includeSelf=tree.path_prefix?.include_self ?? true
      for(const k of ["path_glob","path_prefix","path_eq"]) delete tree[k]
      const treeKind=value(`rule-${prefix}-tree-kind`)
      if(treeKind) tree.kind=treeKind;else delete tree.kind
      if(match) tree[match]=match==="path_prefix"?{path:value(`rule-${prefix}-path`),include_self:includeSelf}:value(`rule-${prefix}-path`)
      if(Object.keys(tree).length) selector.tree=tree;else delete selector.tree
    }
    const relation=value("rule-relation")
    if(relation!=="keep") {
      rule.conditions=relation?[{op:relation}]:[]
      if(relation && relation!=="same_folder") rule.conditions[0].strict=document.getElementById("rule-strict").checked
      if(relation==="resource_below_folder") rule.conditions[0].folder={path:value("rule-folder-path")}
    }
    const index=documentValue.rules.findIndex(r=>r.id===state.editing)
    if(index<0) documentValue.rules.push(rule);else documentValue.rules[index]=rule
    documentValue.schema_version=2;state.editing=rule.id;setDraft(documentValue)
  } catch(error) {showError("editor-error",error)}
})
async function loadTemplates() {
  clearError("template-error")
  try {
    const {templates}=await request("v1/templates"),list=document.getElementById("template-list");list.replaceChildren()
    for(const template of templates) {
      const card=document.createElement("article"),title=document.createElement("h3"),description=document.createElement("p"),button=document.createElement("button")
      title.textContent=template.name;description.textContent=template.description;button.textContent="Use as editable monitor draft";button.className="primary"
      button.addEventListener("click",async()=>{
        try {
          if(state.dirty && !window.confirm("Replace the current unpublished draft?")) return
          const result=await request(`v1/templates/${template.id}`,{method:"POST",body:JSON.stringify({actor_path:value("template-actor"),target_path:value("template-target")})})
          state.mode="monitor";document.querySelectorAll("[data-mode]").forEach(b=>b.classList.toggle("active",b.dataset.mode==="monitor"))
          setDraft(result.document);warnings(result.warnings);document.querySelector('[data-view="editor"]').click()
        } catch(error) {showError("template-error",error)}
      })
      card.append(title,description,button);list.append(card)
    }
  } catch(error) {showError("template-error",error)}
}
async function loadLogs() {
  clearError("logs-error")
  try {const result=await request("v1/policy/decisions");state.decisions=result.decisions;renderLogs()}
  catch(error) {showError("logs-error",error)}
}
function renderLogs() {
  const list=document.getElementById("logs-list");list.replaceChildren()
  for(const record of state.decisions) {
    if(document.getElementById("logs-denials").checked && record.explanation.computed_effect!=="deny") continue
    const card=document.createElement("article"),content=document.createElement("pre");content.textContent=JSON.stringify(record,null,2);card.append(content);list.append(card)
  }
  if(!list.childNodes.length) {const p=document.createElement("p");p.textContent="No matching records in this Proxy's bounded log";list.append(p)}
}
document.getElementById("logs-refresh").addEventListener("click",loadLogs)
document.getElementById("logs-denials").addEventListener("change",renderLogs)

async function loadAuthority() {
  const warning=document.getElementById('authority-warning')
  try {
    const result=await request('v1/policy/authority')
    const settings=document.getElementById('authority-settings')
    settings.hidden=true
    if(typeof result.treer_url==='string') {
      try {
        const treer=new URL(result.treer_url)
        if(['http:','https:'].includes(treer.protocol) && !treer.username && !treer.password) {settings.href=treer.href;settings.hidden=false}
      } catch { /* Core authority remains valid even without a console link. */ }
    }
    if(result.publish_authority==='provider_operator') {
      text('publish-authority','Provider operator')
      warning.textContent='Full policy administration: anyone holding the operator token can publish rules and switch to enforce without further human approval. Use an isolated administration Agent and private token storage. Only the workspace owner can change the Provider or publication authority.'
      warning.classList.remove('hidden')
    } else if(result.publish_authority==='owner_session') {
      text('publish-authority','Owner session')
      warning.textContent='Publication requires the operator token and a current Treer owner session.'
      warning.classList.remove('hidden')
    } else throw new Error('Core publication authority is unsupported')
  } catch(error) {
    document.getElementById('authority-settings').hidden=true
    text('publish-authority','Unavailable')
    warning.textContent='Publication authority could not be verified. Operator-only publication is unavailable; update/check Core before using it.'
    warning.classList.remove('hidden')
  }
}
