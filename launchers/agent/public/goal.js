const statuses = {
  active: ["●", "Active"], paused: ["Ⅱ", "Paused"], blocked: ["!", "Blocked"],
  stale: ["!", "Stale"], done: ["✓", "Done"], cleared: ["○", "Cleared"],
  usageLimited: ["!", "Usage limit reached"], budgetLimited: ["!", "Token budget reached"], complete: ["✓", "Complete"],
};
function node(tag, className, text) {
  const element = document.createElement(tag);
  element.className = className;
  if (text != null) element.textContent = text;
  return element;
}
function button(label, action, callback) {
  const element = node("button", "", label);
  element.type = "button";
  element.dataset.action = action;
  element.addEventListener("click", callback);
  return element;
}

export function setupGoal({ mutate, report, clearPrompt }) {
  const bar = document.querySelector("#goal-bar");
  const editor = document.querySelector("#goal-editor");
  const objective = document.querySelector("#goal-objective");
  const budget = document.querySelector("#goal-budget");
  const errorNode = document.querySelector("#goal-error");
  const save = document.querySelector("#goal-save");
  let state;
  let busy = false;
  let pendingClear = null;
  let previousGoal = null;
  let failedMutation = null;
  const notices = [];

  function openEditor() {
    objective.value = state?.goal?.objective || "";
    budget.value = state?.goal?.tokenBudget ?? "";
    document.querySelector("#goal-editor-title").textContent = state?.goal ? "Edit goal" : "Set goal";
    document.querySelector("#goal-budget-label").hidden = !state?.harness?.capabilities?.goal?.budget;
    errorNode.hidden = true;
    if (!editor.open) editor.showModal();
    objective.focus();
  }
  async function update(method, input) {
    if (busy) return false;
    busy = true;
    save.disabled = true;
    render(state);
    try {
      if (input && method === "PUT") {
        const value = JSON.stringify(input);
        input = { ...input, operationId: failedMutation?.value === value ? failedMutation.id : crypto.randomUUID() };
        failedMutation = { value, id: input.operationId };
      }
      await mutate("api/goal", { method, ...(input ? { body: JSON.stringify(input) } : {}) });
      failedMutation = null;
      return true;
    } finally {
      busy = false;
      save.disabled = false;
      render(state);
    }
  }
  async function clear() {
    const prompt = pendingClear?.prompt;
    try {
      if (await update("DELETE")) {
        pendingClear = null;
        if (prompt != null) clearPrompt(prompt);
      }
    } catch (error) { report(error.message); }
    render(state);
  }
  function confirmClear(prompt = null) {
    pendingClear = { prompt };
    render(state);
    bar.querySelector('[data-action="confirm-clear"]')?.focus();
  }
  function render(snapshot) {
    state = snapshot;
    const capability = state?.harness?.capabilities?.goal;
    bar.hidden = !capability;
    if (!capability) { if (editor.open) editor.close(); pendingClear = null; return; }
    const goal = state.goal;
    if (["complete", "done"].includes(goal?.status) && (!["complete", "done"].includes(previousGoal?.status) || previousGoal?.objective !== goal.objective)) {
      notices.push({ objective: goal.objective, afterTurn: state.thread?.turns?.at(-1)?.id });
      if (notices.length > 20) notices.shift();
    }
    previousGoal = goal ? { objective: goal.objective, status: goal.status } : null;
    const focused = bar.contains(document.activeElement) ? document.activeElement.dataset.action : null;
    bar.replaceChildren();
    bar.className = "goal-bar";
    if (!goal) bar.append(button("Set goal", "set", openEditor));
    else {
      const header = node("div", "goal-heading");
      const status = Object.hasOwn(statuses, goal.status) ? goal.status : "blocked";
      const [icon, label] = statuses[status];
      bar.className = `goal-bar goal-${status}`;
      header.append(node("strong", "goal-objective", goal.objective), node("span", `goal-status goal-status-${status}`, `${icon} ${label}`));
      const tokens = goal.tokensUsed == null ? "Token usage unavailable" : `${Number(goal.tokensUsed).toLocaleString()} tokens used`;
      const seconds = goal.timeUsedSeconds;
      const time = seconds == null ? "Time unavailable" : `${Math.floor(seconds / 60)}m ${Math.floor(seconds % 60)}s used`;
      const metrics = node("p", "goal-metrics", `${tokens} · ${time}`);
      bar.append(header, metrics);
      if (goal.reason) bar.append(node("p", "goal-reason", goal.reason));
      if (goal.requiredAction) bar.append(node("p", "goal-required-action", `Required action: ${goal.requiredAction}`));
      if (goal.rejectionCount != null) bar.append(node("p", "goal-rejections", `Stop hook rejections: ${goal.rejectionCount} (${goal.consecutiveRejections ?? 0} without new evidence)`));
      const delivery = state.goalDelivery;
      if (delivery) {
        bar.append(node("p", "goal-delivery", delivery.status === "sent" ? "Sent to Agent" : delivery.status === "failed" ? `Delivery failed: ${delivery.error || "unknown error"}` : "Sending goal update to Agent…"));
        if (delivery.status === "failed") bar.append(button("Retry goal delivery", "retry-delivery", async () => {
          try { await mutate("api/goal/retry", { method: "POST", body: JSON.stringify({ id: delivery.id }) }); }
          catch (error) { report(error.message); }
        }));
      }
      if (Number.isFinite(goal.tokenBudget) && goal.tokenBudget > 0) {
        const progress = node("progress", "goal-progress");
        progress.max = goal.tokenBudget;
        if (goal.tokensUsed != null) progress.value = Math.min(goal.tokenBudget, Math.max(0, goal.tokensUsed));
        progress.setAttribute("aria-label", "Goal token budget");
        bar.append(progress, node("span", "goal-budget", `${goal.tokensUsed == null ? "—" : Number(goal.tokensUsed).toLocaleString()} / ${Number(goal.tokenBudget).toLocaleString()} tokens`));
      }
      const actions = node("div", "goal-actions");
      actions.append(button("Edit goal", "edit", openEditor));
      if (capability.pause && ["active", "paused"].includes(goal.status)) {
        actions.append(button(goal.status === "paused" ? "Resume goal" : "Pause goal", "pause", async () => {
          try { await update("PUT", { status: goal.status === "paused" ? "active" : "paused" }); }
          catch (error) { report(error.message); }
        }));
      }
      actions.append(button("Clear goal", "clear", () => confirmClear()));
      bar.append(actions);
    }
    if (pendingClear) {
      const confirmation = node("div", "goal-confirm");
      confirmation.setAttribute("role", "group");
      confirmation.setAttribute("aria-label", "Confirm clear goal");
      confirmation.append(node("span", "", "Clear this goal?"), button("Confirm clear goal", "confirm-clear", () => void clear()), button("Keep goal", "keep", () => { pendingClear = null; render(state); }));
      bar.append(confirmation);
    }
    for (const action of bar.querySelectorAll("button")) action.disabled = busy;
    if (focused) bar.querySelector(`[data-action="${focused}"]`)?.focus();
  }
  document.querySelector("#goal-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    const input = { objective: objective.value.trim() };
    if (!input.objective || busy) return;
    if (state?.harness?.capabilities?.goal?.budget) {
      input.tokenBudget = budget.value === "" ? null : Number(budget.value);
      if (input.tokenBudget != null && (!Number.isSafeInteger(input.tokenBudget) || input.tokenBudget <= 0)) {
        errorNode.textContent = "Token budget must be a positive whole number.";
        errorNode.hidden = false;
        return;
      }
    }
    errorNode.hidden = true;
    try { if (await update("PUT", input)) editor.close(); }
    catch (error) { errorNode.textContent = error.message; errorNode.hidden = false; }
  });
  for (const id of ["goal-editor-close", "goal-editor-cancel"]) document.querySelector(`#${id}`).addEventListener("click", () => editor.close());
  return {
    render, notices,
    async intercept(prompt) {
      if (!/^\/goal(?:\s|$)/.test(prompt)) return null;
      if (!state?.harness?.capabilities?.goal) throw new Error("This harness does not support goals.");
      const objective = prompt.slice(5).trim();
      if (!objective) { openEditor(); return { clear: false }; }
      if (objective === "clear") { confirmClear(prompt); return { clear: false }; }
      return { clear: await update("PUT", { objective }) };
    },
  };
}
