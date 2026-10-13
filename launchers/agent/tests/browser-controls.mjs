import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { expect } from "@playwright/test";
import { WebSocketServer } from "ws";

// Pure HTTP/WS contract fixture: never starts a harness or reads Agent credentials.
export async function testRuntimeControls(browser, screenshots) {
  let state = {
    type: "state", ready: true, runtime: { state: "idle" }, modelOptions: [],
    harness: { name: "Mock Agent", capabilities: { steer: true, backgroundTasks: true, goal: { pause: true, budget: true } } },
    history: { limit: 50 }, thread: { id: "mock-thread", title: "Runtime controls", cwd: "/workspace/mock", status: "idle", turns: [] },
    backgroundTasks: [], queue: [], goal: null,
  };
  const prompts = [];
  const goalWrites = [];
  const outputReads = [];
  const canceled = [];
  let failPrompt = false;
  let failGoal = false;
  let failCancel = false;
  let outputFormat = "text";
  let failOutput = false;
  const malicious = '<img src=x onerror="window.controlsXSS=true">';
  let sequence = 0;
  const wss = new WebSocketServer({ noServer: true });
  const broadcast = () => { for (const client of wss.clients) if (client.readyState === 1) client.send(JSON.stringify(state)); };
  const setState = (patch) => { state = { ...state, ...patch }; broadcast(); };
  const busy = (working) => setState({ thread: { ...state.thread, status: working ? "running" : "idle" }, runtime: { state: working ? "working" : "idle" } });
  const server = createServer(async (req, res) => {
    const url = new URL(req.url, "http://localhost");
    const path = url.pathname.replace(/^\/mock\//, "");
    const json = (value, status = 200) => { res.writeHead(status, { "content-type": "application/json" }); res.end(JSON.stringify(value)); };
    try {
      if (path === "api/state") return json(state);
      if (path === "api/prompt" && req.method === "POST") {
        let body = "";
        for await (const chunk of req) body += chunk;
        const prompt = JSON.parse(body);
        prompts.push(prompt);
        if (failPrompt) return json({ error: { message: "Prompt transport failed" } }, 503);
        if (state.thread.status === "running" && prompt.mode === "queue") {
          state.queue.push({ id: `q-${++sequence}`, text: prompt.prompt, createdAt: new Date().toISOString() });
        }
        broadcast();
        return json(state);
      }
      if (path.startsWith("api/queue/") && req.method === "DELETE") {
        if (failCancel) return json({ error: { message: "Queue cancellation failed" } }, 503);
        const id = decodeURIComponent(path.slice("api/queue/".length));
        canceled.push(id);
        state.queue = state.queue.filter((entry) => entry.id !== id);
        broadcast();
        // Mutations can acknowledge without returning a full state snapshot.
        res.writeHead(204);
        return res.end();
      }
      if (path.startsWith("api/tasks/")) {
        outputReads.push({ path, tail: url.searchParams.get("tail") });
        if (failOutput) return json({ error: "Output transport failed" }, 503);
        const content = `Output ${outputReads.length}\n${malicious}`;
        if (outputFormat === "json") return json({ content, size: content.length, truncated: false });
        res.writeHead(200, { "content-type": "text/plain" });
        return res.end(content);
      }
      if (path === "api/goal/reconcile") {
        state.goal = { ...state.goal, syncStatus: "synced", nativeObjective: state.goal.objective, syncError: undefined }; broadcast(); return json({ goal: state.goal });
      }
      if (path === "api/goal/retry") {
        state.goalDelivery = { ...state.goalDelivery, status: "sent", error: null }; broadcast(); return json({ goalDelivery: state.goalDelivery });
      }
      if (path === "api/goal") {
        if (failGoal) return json({ error: { message: "Goal transport failed" } }, 503);
        if (req.method === "DELETE") { goalWrites.push({ method: "DELETE" }); state.goal = null; }
        else {
          let body = "";
          for await (const chunk of req) body += chunk;
          const input = JSON.parse(body);
          goalWrites.push({ method: "PUT", ...input });
          state.goal = { objective: "", status: "active", tokensUsed: 0, timeUsedSeconds: 0, tokenBudget: null,
            ...state.goal, ...input, updatedAt: new Date().toISOString() };
        }
        broadcast();
        return json({ goal: state.goal });
      }
      if (path === "v1/files/tree") return json({ entries: [] });
      const file = path === "" ? "index.html" : path;
      if (!/^[a-z-]+\.(?:html|css|js)$/.test(file)) return json({ error: "not found" }, 404);
      const content = await readFile(join(import.meta.dirname, "../public", file));
      res.writeHead(200, { "content-type": file.endsWith("js") ? "application/javascript" : file.endsWith("css") ? "text/css" : "text/html" });
      res.end(content);
    } catch (error) { json({ error: error.message }, 500); }
  });
  server.on("upgrade", (req, socket, head) => wss.handleUpgrade(req, socket, head, (client) => client.send(JSON.stringify(state))));
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const prompt = page.locator("#prompt");
  try {
    await page.goto(`http://127.0.0.1:${server.address().port}/mock/?theme=light`);
    await expect(page.locator("#send")).toHaveAttribute("aria-label", "Send message");
    await prompt.fill("idle Enter");
    await prompt.press("Enter");
    await expect.poll(() => prompts.length).toBe(1);
    assert.equal(prompts.at(-1).mode, "steer");
    await expect(prompt).toHaveValue("");
    busy(true);
    await expect(page.locator("#send")).toHaveAttribute("aria-label", "Steer current run");
    await expect(page.locator(".send-actions button:visible")).toHaveCount(2);
    await expect(page.locator("#send")).toHaveText("Steer");
    await expect(page.locator("#queue-send")).toBeVisible();
    await expect(page.locator("#queue-send")).toHaveText("Queue");
    await prompt.fill("live Enter");
    await prompt.press("Enter");
    await expect.poll(() => prompts.length).toBe(2);
    assert.equal(prompts.at(-1).mode, "steer");
    await expect(prompt).toHaveValue("");
    await prompt.fill("Tab queued");
    await prompt.press("Tab");
    await expect(page.locator("#prompt-queue")).toContainText("Tab queued");
    assert.equal(prompts.at(-1).mode, "queue");
    await prompt.fill("button steer");
    await page.getByRole("button", { name: "Steer current run", exact: true }).click();
    await expect.poll(() => prompts.at(-1)?.prompt).toBe("button steer");
    assert.equal(prompts.at(-1).mode, "steer");
    await expect(prompt).toHaveValue("");
    await prompt.fill("button queued");
    await page.getByRole("button", { name: "Queue message", exact: true }).click();
    await expect(page.locator("#prompt-queue .queued-prompt")).toHaveCount(2);
    assert.equal(prompts.at(-1).mode, "queue");
    failCancel = true;
    await page.getByRole("button", { name: "Cancel queued message: Tab queued", exact: true }).click();
    await expect(page.locator("#feedback")).toHaveText("Queue cancellation failed");
    await expect(page.locator("#prompt-queue .queued-prompt")).toHaveCount(2);
    failCancel = false;
    await page.getByRole("button", { name: "Cancel queued message: Tab queued", exact: true }).click();
    await expect(page.locator("#prompt-queue .queued-prompt")).toHaveCount(1);
    assert.equal(canceled.length, 1);
    // Drained entries disappear on a WebSocket update; the UI never dispatches them itself.
    setState({ queue: [] });
    await expect(page.locator("#prompt-queue")).toBeHidden();
    setState({ queue: [{ id: "source-test", text: malicious, createdAt: new Date().toISOString(), sender: { agent_id: "ag_leader", path: "/treer-dev-team/leader", name: "leader" }, position: 1 }] });
    await expect(page.locator("#prompt-queue")).toContainText("Queued 1 · /treer-dev-team/leader");
    await expect(page.locator("#prompt-queue .queued-text")).toHaveText(malicious);
    await expect(page.locator("#prompt-queue img")).toHaveCount(0);
    await page.screenshot({ path: join(screenshots, "controls-desktop-queue-source.png") });
    setState({ queue: [] });
    failPrompt = true;
    await prompt.fill("keep this draft");
    await prompt.press("Enter");
    await expect(page.locator("#feedback")).toHaveText("Prompt transport failed");
    await expect(prompt).toHaveValue("keep this draft");
    failPrompt = false;
    setState({ harness: { ...state.harness, capabilities: { ...state.harness.capabilities, steer: false } } });
    await expect(page.locator("#send")).toHaveAttribute("aria-label", "Queue message (steering unavailable)");
    await expect(page.locator(".send-actions button:visible")).toHaveCount(1);
    await expect(page.locator("#send")).toHaveText("Queue");
    await expect(page.locator("#queue-send")).toBeHidden();
    await expect(page.locator("#prompt-shortcuts")).toContainText("Enter to queue (steering unavailable)");
    await prompt.press("Enter");
    await expect(page.locator("#feedback")).toContainText("does not support steering");
    assert.equal(prompts.at(-1).mode, "queue");
    await prompt.fill("line one");
    const countBeforeNewline = prompts.length;
    await prompt.press("Shift+Enter");
    await expect(prompt).toHaveValue("line one\n");
    await prompt.dispatchEvent("keydown", { key: "Enter", isComposing: true });
    assert.equal(prompts.length, countBeforeNewline);
    await prompt.fill("");
    await prompt.press("Tab");
    assert.notEqual(await page.evaluate(() => document.activeElement.id), "prompt", "Empty Tab retains keyboard focus navigation");

    const startedAt = new Date(Date.now() - 125000).toISOString();
    setState({ backgroundTasks: [
      { id: "task/a", kind: "shell", description: `Build ${malicious}`, status: "running", startedAt, endedAt: null, exitCode: null, summary: "Compiling" },
      { id: "done", kind: "agent", description: "Research", status: "completed", startedAt, endedAt: new Date().toISOString(), exitCode: 0, summary: "Research complete" },
      { id: "failed", kind: "monitor", description: "Watch tests", status: "failed", startedAt, endedAt: new Date().toISOString(), exitCode: 2, summary: malicious },
      { id: "stopped", kind: "other", description: "Stopped job", status: "stopped", startedAt, endedAt: new Date().toISOString(), exitCode: null, summary: "Stopped by user" },
    ] });
    await expect(page.locator("#tasks-toggle")).toHaveText("1 running");
    assert.equal(outputReads.length, 0, "Outputs load only after expansion");
    await page.locator("#tasks-toggle").click();
    await expect(page.locator(".task-card")).toHaveCount(4);
    const running = page.locator(".task-running");
    await running.locator("summary").click();
    await expect(running.locator(".task-output")).toContainText(malicious);
    assert.equal(outputReads[0].tail, "65536");
    assert.ok(outputReads[0].path.includes("task%2Fa"));
    await expect.poll(() => outputReads.length, { timeout: 6000 }).toBeGreaterThan(1);
    await expect(running).toHaveAttribute("open", "");
    setState({ backgroundTasks: state.backgroundTasks.map((task) => task.id === "task/a" ? { ...task, status: "completed", exitCode: 0, endedAt: new Date().toISOString(), summary: "Build complete" } : task) });
    const completed = page.locator(".task-card").filter({ hasText: "Build complete" });
    await expect(completed).toHaveClass(/task-completed/);
    await expect(completed.locator(".task-meta")).toContainText("Exit 0");
    await expect.poll(() => outputReads.length).toBeGreaterThan(2);
    const finalReads = outputReads.length;
    await page.waitForTimeout(3400);
    assert.equal(outputReads.length, finalReads, "Completed tasks stop polling");
    outputFormat = "json";
    const failed = page.locator(".task-failed");
    await failed.locator("summary").click();
    await expect(failed.locator(".task-output")).toContainText(malicious);
    await expect(failed.locator(".task-meta")).toContainText("Exit 2");
    assert.equal(await page.locator("#tasks-panel img").count(), 0);
    assert.equal(await page.evaluate(() => window.controlsXSS), undefined);
    failOutput = true;
    await failed.getByRole("button", { name: "Refresh output" }).click();
    await expect(failed.locator(".task-error")).toContainText("Output unavailable");
    failOutput = false;
    await failed.getByRole("button", { name: "Refresh output" }).click();
    await expect(failed.locator(".task-error")).toBeHidden();
    await page.screenshot({ path: join(screenshots, "controls-desktop-tasks.png") });
    await page.getByRole("button", { name: "Close background tasks" }).click();

    await page.getByRole("button", { name: "Set goal", exact: true }).click();
    await page.getByLabel("Objective", { exact: true }).fill(`Ship ${malicious}`);
    await page.getByLabel("Token budget (optional)").fill("1000");
    failGoal = true;
    await page.getByRole("button", { name: "Save goal" }).click();
    await expect(page.locator("#goal-error")).toHaveText("Goal transport failed");
    await expect(page.getByLabel("Objective", { exact: true })).toHaveValue(`Ship ${malicious}`);
    failGoal = false;
    await page.getByRole("button", { name: "Save goal" }).click();
    await expect(page.locator("#goal-editor")).not.toBeVisible();
    await expect(page.locator(".goal-status")).toHaveText("● Active");
    assert.equal(goalWrites.at(-1).tokenBudget, 1000);
    setState({ goal: { ...state.goal, tokensUsed: 250, timeUsedSeconds: 90 } });
    await expect(page.locator(".goal-metrics")).toContainText("250 tokens used · 1m 30s used");
    await expect(page.getByRole("progressbar", { name: "Goal token budget" })).toHaveAttribute("value", "250");
    await page.getByRole("button", { name: "Pause goal", exact: true }).click();
    await expect(page.locator(".goal-status")).toHaveText("Ⅱ Paused");
    await page.getByRole("button", { name: "Resume goal", exact: true }).click();
    await expect(page.locator(".goal-status")).toHaveText("● Active");
    await page.getByRole("button", { name: "Edit goal", exact: true }).click();
    await expect(page.getByLabel("Objective", { exact: true })).toHaveValue(`Ship ${malicious}`);
    await expect(page.getByLabel("Token budget (optional)")).toHaveValue("1000");
    await page.getByLabel("Token budget (optional)").fill("");
    await page.getByRole("button", { name: "Save goal" }).click();
    await expect(page.locator("#goal-editor")).not.toBeVisible();
    assert.equal(goalWrites.at(-1).tokenBudget, null);
    await expect(page.getByRole("progressbar", { name: "Goal token budget" })).toHaveCount(0);
    for (const [status, label] of [["blocked", "Blocked"], ["usageLimited", "Usage limit reached"], ["budgetLimited", "Token budget reached"], ["complete", "Complete"]]) {
      setState({ goal: { ...state.goal, status } });
      await expect(page.locator(".goal-status")).toContainText(label);
      await expect(page.locator("#goal-bar")).toHaveClass(new RegExp(`goal-${status}`));
    }
    await expect(page.locator(".goal-completion")).toHaveCount(1);
    await expect(page.locator(".goal-completion")).toContainText(`Goal complete: Ship ${malicious}`);
    broadcast();
    await expect(page.locator(".goal-completion")).toHaveCount(1);
    await page.screenshot({ path: join(screenshots, "controls-desktop-complete.png") });
    setState({ goal: { ...state.goal, status: "blocked", reason: `External owner needed ${malicious}`, requiredAction: "Owner: create the deployment environment", rejectionCount: 3, consecutiveRejections: 3 }, goalDelivery: { id: "edit-visible", status: "sent" } });
    await expect(page.locator(".goal-reason")).toContainText(`External owner needed ${malicious}`);
    await expect(page.locator(".goal-required-action")).toContainText("Owner: create the deployment environment");
    await expect(page.locator(".goal-rejections")).toContainText("3 (3 without new evidence)");
    await expect(page.locator(".goal-delivery")).toHaveText("Sent to Agent");
    setState({ goalDelivery: { id: "edit-visible", status: "failed", error: "Temporary send failure" } });
    await expect(page.locator(".goal-delivery")).toContainText("Temporary send failure");
    await page.getByRole("button", { name: "Retry goal delivery", exact: true }).click();
    await expect(page.locator(".goal-delivery")).toHaveText("Sent to Agent");
    await page.screenshot({ path: join(screenshots, "controls-desktop-blocked.png") });
    setState({ goal: { ...state.goal, status: "active", syncStatus: "desynced", nativeObjective: "Previous condition", syncError: `Waiting for idle ${malicious}` } });
    await expect(page.locator(".goal-sync")).toContainText(`Waiting for idle ${malicious}`);
    await page.screenshot({ path: join(screenshots, "controls-desktop-desynced.png") });
    await page.getByRole("button", { name: "Re-align native goal", exact: true }).click();
    await expect(page.locator(".goal-sync")).toHaveCount(0);
    setState({ goal: { ...state.goal, syncStatus: "exit_unconfirmed", syncError: "Native clear was not acknowledged. Please manually enter /goal clear." } });
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(page.locator(".goal-sync")).toContainText("manually enter /goal clear");
    await expect(page.getByRole("button", { name: "Clear goal", exact: true })).toBeEnabled();
    await page.screenshot({ path: join(screenshots, "controls-mobile-unconfirmed.png") });
    await page.setViewportSize({ width: 1440, height: 1000 });
    await page.getByRole("button", { name: "Clear goal", exact: true }).click();
    assert.notEqual(goalWrites.at(-1).method, "DELETE");
    await page.getByRole("button", { name: "Keep goal", exact: true }).click();
    await expect(page.locator(".goal-confirm")).toHaveCount(0);
    await page.getByRole("button", { name: "Clear goal", exact: true }).click();
    await page.getByRole("button", { name: "Confirm clear goal", exact: true }).click();
    await expect(page.getByRole("button", { name: "Set goal", exact: true })).toBeVisible();
    const promptCount = prompts.length;
    await prompt.fill("/goal finish the release");
    await prompt.press("Enter");
    await expect(page.locator(".goal-objective")).toHaveText("finish the release");
    assert.equal(prompts.length, promptCount);
    await prompt.fill("/goal clear");
    await prompt.press("Enter");
    await expect(page.locator(".goal-confirm")).toBeVisible();
    await expect(prompt).toHaveValue("/goal clear");
    failGoal = true;
    await page.getByRole("button", { name: "Confirm clear goal", exact: true }).click();
    await expect(page.locator("#feedback")).toHaveText("Goal transport failed");
    await expect(prompt).toHaveValue("/goal clear");
    failGoal = false;
    await page.getByRole("button", { name: "Confirm clear goal", exact: true }).click();
    await expect(prompt).toHaveValue("");
    assert.equal(prompts.length, promptCount);
    failGoal = true;
    await prompt.fill("/goal keep this objective");
    await prompt.press("Enter");
    await expect(prompt).toHaveValue("/goal keep this objective");
    failGoal = false;
    setState({ harness: { ...state.harness, capabilities: { ...state.harness.capabilities, goal: { pause: false, budget: false } } }, goal: { objective: "Unbudgeted goal", status: "active", tokensUsed: null, timeUsedSeconds: null, tokenBudget: null } });
    await expect(page.getByRole("button", { name: "Pause goal", exact: true })).toHaveCount(0);
    await expect(page.locator(".goal-metrics")).toContainText("Token usage unavailable");
    await page.getByRole("button", { name: "Edit goal", exact: true }).click();
    await expect(page.locator("#goal-budget-label")).toBeHidden();
    await page.getByRole("button", { name: "Cancel", exact: true }).click();

    await page.setViewportSize({ width: 390, height: 844 });
    for (const [status, label] of [["stale", "Stale"], ["done", "Done"], ["cleared", "Cleared"], ["blocked", "Blocked"]]) {
      setState({ goal: { ...state.goal, status, reason: "Waiting for owner action", requiredAction: "Owner: provision environment" } });
      await expect(page.locator(".goal-status")).toContainText(label);
    }
    await expect(page.locator(".goal-reason")).toContainText("Waiting for owner action");
    await page.screenshot({ path: join(screenshots, "controls-mobile-blocked.png") });
    await prompt.fill("touch queued");
    await page.getByRole("button", { name: "Queue message (steering unavailable)", exact: true }).click();
    await expect(page.locator("#prompt-queue")).toContainText("touch queued");
    setState({ queue: state.queue.map((entry, index) => ({ ...entry, position: index + 1, sender: { agent_id: "ag_leader", path: "/treer-dev-team/leader", name: "leader" } })) });
    await expect(page.locator("#prompt-queue")).toContainText("/treer-dev-team/leader");
    await page.screenshot({ path: join(screenshots, "controls-mobile-goal-queue.png") });
    await page.getByRole("button", { name: "Browse files", exact: true }).click();
    await expect(page.locator("#file-browser")).toBeVisible();
    await page.locator("#tasks-toggle").click();
    await expect(page.locator("#tasks-panel")).toBeVisible();
    await expect(page.locator("#file-browser")).toBeHidden();
    assert.ok(await page.evaluate(() => document.querySelector("#tasks-list").getBoundingClientRect().bottom <= document.querySelector("#composer").getBoundingClientRect().top), "The task drawer stays above the composer");
    await page.screenshot({ path: join(screenshots, "controls-mobile-tasks.png") });
    await page.getByRole("button", { name: "Browse files", exact: true }).click();
    await expect(page.locator("#tasks-panel")).toBeHidden();
    await expect(page.locator("#file-browser")).toBeVisible();
    await page.getByRole("button", { name: "Close file browser" }).click();
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "No mobile horizontal overflow");
    setState({ harness: { ...state.harness, capabilities: { steer: false, backgroundTasks: false, goal: null } } });
    await expect(page.locator("#tasks-toggle")).toBeHidden();
    await expect(page.locator("#goal-bar")).toBeHidden();
    await prompt.fill("/goal unsupported");
    await prompt.press("Enter");
    await expect(page.locator("#feedback")).toContainText("does not support goals");
    assert.equal(prompts.at(-1).prompt, "touch queued");
    assert.equal(await page.evaluate(() => window.controlsXSS), undefined);
    assert.deepEqual(errors, []);
    console.log("Runtime controls mock browser checks passed (tasks / queue / goal, 1440px and 390px).");
  } finally {
    await context.close();
    for (const client of wss.clients) client.terminate();
    await new Promise((resolve) => wss.close(resolve));
    await new Promise((resolve) => server.close(resolve));
  }
}
