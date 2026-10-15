import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { expect } from "@playwright/test";
import { WebSocketServer } from "ws";

// The same pure HTTP/WS contract fixture as the former browser-controls.mjs, driving the React UI. Never starts a harness or reads credentials.
async function phoneNav(page, item) {
  await page.getByRole("button", { name: "Menu", exact: true }).click();
  await page.getByRole("menuitem", { name: item, exact: true }).click();
}

export async function testRuntimeControlsUi(browser, screenshots, pageName = "index.html") {
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
      const file = path === "" ? pageName : path;
      if (!/^[a-z-]+\.(?:html|css|js)$/.test(file)) return json({ error: "not found" }, 404);
      const content = await readFile(join(import.meta.dirname, "../public", file));
      res.writeHead(200, { "content-type": file.endsWith("js") ? "application/javascript" : file.endsWith("css") ? "text/css" : "text/html" });
      res.end(content);
    } catch (error) { json({ error: error.message }, 500); }
  });
  server.on("upgrade", (req, socket, head) => wss.handleUpgrade(req, socket, head, (client) => client.send(JSON.stringify(state))));
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
  await context.addInitScript(() => { window.__violations = []; document.addEventListener("securitypolicyviolation", (event) => window.__violations.push(event.violatedDirective)); });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => { if (message.type() === "error" && !/inline event handler/.test(message.text())) errors.push(message.text()); });
  const prompt = page.getByRole("textbox", { name: /Message/ });
  const strip = page.getByRole("toolbar", { name: "Run status" });
  const feedback = page.locator(".feedback");
  const goalSegment = () => strip.getByRole("button", { name: /^Goal/ });
  const goalPop = page.locator("#pop-goal");
  const openGoal = async () => { if (!(await goalPop.isVisible())) await goalSegment().click(); };
  try {
    await page.goto(`http://127.0.0.1:${server.address().port}/mock/${pageName}?theme=light&look=b`);
    await expect(page.getByRole("button", { name: "Send message" })).toBeDisabled();
    await prompt.fill("idle Enter");
    await expect(page.getByRole("button", { name: "Send message" })).toBeEnabled();
    await prompt.press("Enter");
    await expect.poll(() => prompts.length).toBe(1);
    assert.equal(prompts.at(-1).mode, "steer");
    await expect(prompt).toHaveValue("");
    busy(true);
    await expect(page.getByRole("button", { name: "Steer current run" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Queue", exact: true })).toBeVisible();
    await prompt.fill("live Enter");
    await prompt.press("Enter");
    await expect.poll(() => prompts.length).toBe(2);
    assert.equal(prompts.at(-1).mode, "steer");
    await expect(prompt).toHaveValue("");
    await prompt.fill("Tab queued");
    await prompt.press("Tab");
    await expect(strip.getByRole("button", { name: /Queue/ })).toContainText("1");
    assert.equal(prompts.at(-1).mode, "queue");
    await prompt.fill("button steer");
    await page.getByRole("button", { name: "Steer current run", exact: true }).click();
    await expect.poll(() => prompts.at(-1)?.prompt).toBe("button steer");
    assert.equal(prompts.at(-1).mode, "steer");
    await expect(prompt).toHaveValue("");
    await prompt.fill("button queued");
    await page.getByRole("button", { name: "Queue", exact: true }).click();
    await expect(strip.getByRole("button", { name: /Queue/ })).toContainText("2");
    assert.equal(prompts.at(-1).mode, "queue");
    await strip.getByRole("button", { name: /Queue/ }).click();
    const queuePop = page.locator("#pop-queue");
    failCancel = true;
    await queuePop.getByRole("button", { name: "Cancel queued message: Tab queued", exact: true }).click();
    await expect(feedback).toHaveText("Queue cancellation failed");
    await expect(queuePop.locator(".qrow")).toHaveCount(2);
    failCancel = false;
    // The cancel mutation acknowledges with 204 and no snapshot; the UI refetches state.
    await queuePop.getByRole("button", { name: "Cancel queued message: Tab queued", exact: true }).click();
    await expect(queuePop.locator(".qrow")).toHaveCount(1);
    assert.equal(canceled.length, 1);
    await expect(feedback).toHaveCount(0); // a 204 acknowledgement is a success, not an error
    // Drained entries disappear on a WebSocket update; the UI never dispatches them itself.
    setState({ queue: [] });
    await expect(strip.getByRole("button", { name: /Queue/ })).toHaveCount(0);
    setState({ queue: [{ id: "source-test", text: malicious, createdAt: new Date().toISOString(), sender: { agent_id: "ag_leader", path: "/treer-dev-team/leader", name: "leader" }, position: 1 }] });
    await strip.getByRole("button", { name: /Queue/ }).click();
    await expect(queuePop).toContainText("From /treer-dev-team/leader");
    await expect(queuePop.locator(".qt")).toContainText(malicious);
    await expect(queuePop.locator("img")).toHaveCount(0);
    await page.screenshot({ path: join(screenshots, "controls-ui-queue-source.png") });
    await page.keyboard.press("Escape");
    setState({ queue: [] });
    failPrompt = true;
    await prompt.fill("keep this draft");
    await prompt.press("Enter");
    await expect(feedback).toHaveText("Prompt transport failed");
    await expect(prompt).toHaveValue("keep this draft");
    failPrompt = false;
    setState({ harness: { ...state.harness, capabilities: { ...state.harness.capabilities, steer: false } } });
    await expect(page.getByRole("button", { name: "Queue message" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Queue", exact: true })).toHaveCount(0);
    await expect(page.locator("#hint")).toContainText("Enter to queue (steering unavailable)");
    await prompt.press("Enter");
    await expect(feedback).toContainText("does not support steering");
    assert.equal(prompts.at(-1).mode, "queue");
    await prompt.fill("line one");
    const countBeforeNewline = prompts.length;
    await prompt.press("Shift+Enter");
    await expect(prompt).toHaveValue("line one\n");
    await prompt.dispatchEvent("keydown", { key: "Enter", isComposing: true });
    assert.equal(prompts.length, countBeforeNewline);
    await prompt.fill("");
    await prompt.press("Tab");
    assert.notEqual(await page.evaluate(() => document.activeElement.id), "ta", "Empty Tab retains keyboard focus navigation");

    const startedAt = new Date(Date.now() - 125000).toISOString();
    setState({ backgroundTasks: [
      { id: "task/a", kind: "shell", description: `Build ${malicious}`, status: "running", startedAt, endedAt: null, exitCode: null, summary: "Compiling" },
      { id: "done", kind: "agent", description: "Research", status: "completed", startedAt, endedAt: new Date().toISOString(), exitCode: 0, summary: "Research complete" },
      { id: "failed", kind: "monitor", description: "Watch tests", status: "failed", startedAt, endedAt: new Date().toISOString(), exitCode: 2, summary: malicious },
      { id: "stopped", kind: "other", description: "Stopped job", status: "stopped", startedAt, endedAt: new Date().toISOString(), exitCode: null, summary: "Stopped by user" },
    ] });
    const tasksButton = page.getByRole("button", { name: /^Background tasks/ });
    await expect(tasksButton).toContainText("1 running");
    assert.equal(outputReads.length, 0, "Outputs load only after expansion");
    await tasksButton.click();
    await expect(page.locator(".task")).toHaveCount(4);
    const running = page.locator(".task-running");
    await running.locator("summary").click();
    await expect(running.locator(".task-output")).toContainText(malicious);
    assert.equal(outputReads[0].tail, "65536");
    assert.ok(outputReads[0].path.includes("task%2Fa"));
    await expect.poll(() => outputReads.length, { timeout: 6000 }).toBeGreaterThan(1);
    await expect(running).toHaveAttribute("open", "");
    setState({ backgroundTasks: state.backgroundTasks.map((task) => task.id === "task/a" ? { ...task, status: "completed", exitCode: 0, endedAt: new Date().toISOString(), summary: "Build complete" } : task) });
    const completed = page.locator(".task").filter({ hasText: "Build complete" });
    await expect(completed).toHaveClass(/task-completed/);
    await expect(completed.locator(".tm")).toContainText("Exit 0");
    await expect.poll(() => outputReads.length).toBeGreaterThan(2);
    const finalReads = outputReads.length;
    await page.waitForTimeout(3400);
    assert.equal(outputReads.length, finalReads, "Completed tasks stop polling");
    outputFormat = "json";
    const failed = page.locator(".task-failed");
    await failed.locator("summary").click();
    await expect(failed.locator(".task-output")).toContainText(malicious);
    await expect(failed.locator(".tm")).toContainText("Exit 2");
    assert.equal(await page.locator("#tasks img").count(), 0);
    assert.equal(await page.evaluate(() => window.controlsXSS), undefined);
    failOutput = true;
    await failed.getByRole("button", { name: "Refresh output" }).click();
    await expect(failed.locator(".field-error")).toContainText("Output unavailable");
    failOutput = false;
    await failed.getByRole("button", { name: "Refresh output" }).click();
    await expect(failed.locator(".field-error")).toHaveCount(0);
    await page.screenshot({ path: join(screenshots, "controls-ui-tasks.png") });
    await page.getByRole("button", { name: "Close background tasks" }).click();

    // Goal: editor failure keeps the form, then success; metrics, pause/resume, budget removal.
    await strip.getByRole("button", { name: "Set goal", exact: true }).click();
    const editor = page.locator("dialog.goal-dialog");
    await editor.getByLabel("Objective", { exact: true }).fill(`Ship ${malicious}`);
    await editor.getByLabel("Token budget (optional)").fill("1000");
    failGoal = true;
    await editor.getByRole("button", { name: "Save goal" }).click();
    await expect(editor.getByRole("alert")).toHaveText("Goal transport failed");
    await expect(editor.getByLabel("Objective", { exact: true })).toHaveValue(`Ship ${malicious}`);
    failGoal = false;
    await editor.getByRole("button", { name: "Save goal" }).click();
    await expect(editor).toBeHidden();
    await openGoal();
    await expect(goalPop.locator(".goal-status")).toHaveText("Active");
    assert.equal(goalWrites.at(-1).tokenBudget, 1000);
    setState({ goal: { ...state.goal, tokensUsed: 250, timeUsedSeconds: 90 } });
    await expect(goalPop.locator(".meta").first()).toContainText("250 tokens used · 1m 30s used");
    await expect(goalPop.getByRole("progressbar", { name: "Goal token budget" })).toHaveAttribute("value", "250");
    await goalPop.getByRole("button", { name: "Pause goal", exact: true }).click();
    await expect(goalPop.locator(".goal-status")).toHaveText("Paused");
    await goalPop.getByRole("button", { name: "Resume goal", exact: true }).click();
    await expect(goalPop.locator(".goal-status")).toHaveText("Active");
    await goalPop.getByRole("button", { name: "Edit goal", exact: true }).click();
    await expect(editor.getByLabel("Objective", { exact: true })).toHaveValue(`Ship ${malicious}`);
    await expect(editor.getByLabel("Token budget (optional)")).toHaveValue("1000");
    await editor.getByLabel("Token budget (optional)").fill("");
    await editor.getByRole("button", { name: "Save goal" }).click();
    await expect(editor).toBeHidden();
    assert.equal(goalWrites.at(-1).tokenBudget, null);
    await openGoal();
    await expect(goalPop.getByRole("progressbar", { name: "Goal token budget" })).toHaveCount(0);
    for (const [status, label] of [["blocked", "Blocked"], ["usageLimited", "Usage limit reached"], ["budgetLimited", "Token budget reached"], ["complete", "Complete"]]) {
      setState({ goal: { ...state.goal, status } });
      await openGoal();
      await expect(goalPop.locator(".goal-status")).toContainText(label);
      if (status !== "complete") await expect(goalSegment()).toHaveClass(/attn/);
    }
    await expect(page.locator(".goal-done")).toHaveCount(1);
    await expect(page.locator(".goal-done")).toContainText(`Goal complete: Ship ${malicious}`);
    broadcast();
    await expect(page.locator(".goal-done")).toHaveCount(1);
    await page.screenshot({ path: join(screenshots, "controls-ui-complete.png") });
    setState({ goal: { ...state.goal, status: "blocked", reason: `External owner needed ${malicious}`, requiredAction: "Owner: create the deployment environment", rejectionCount: 3, consecutiveRejections: 3 }, goalDelivery: { id: "edit-visible", status: "sent" } });
    await openGoal();
    await expect(goalPop).toContainText(`External owner needed ${malicious}`);
    await expect(goalPop).toContainText("Owner: create the deployment environment");
    await expect(goalPop).toContainText("3 (3 without new evidence)");
    await expect(goalPop).toContainText("Sent to Agent");
    setState({ goalDelivery: { id: "edit-visible", status: "failed", error: "Temporary send failure" } });
    await expect(goalPop).toContainText("Temporary send failure");
    await goalPop.getByRole("button", { name: "Retry goal delivery", exact: true }).click();
    await expect(goalPop).toContainText("Sent to Agent");
    await page.screenshot({ path: join(screenshots, "controls-ui-blocked.png") });
    setState({ goal: { ...state.goal, status: "active", syncStatus: "desynced", nativeObjective: "Previous condition", syncError: `Waiting for idle ${malicious}` } });
    await openGoal();
    await expect(goalPop).toContainText(`Waiting for idle ${malicious}`);
    assert.equal(await page.evaluate(() => window.controlsXSS), undefined, "syncError must be rendered as text");
    await expect(goalPop.locator("img")).toHaveCount(0);
    await goalPop.getByRole("button", { name: "Re-align native goal", exact: true }).click();
    await expect(goalPop.locator(".goal-warn")).toHaveCount(0);
    setState({ goal: { ...state.goal, syncStatus: "exit_unconfirmed", syncError: "Native clear was not acknowledged. Please manually enter /goal clear." } });
    await page.setViewportSize({ width: 390, height: 844 });
    await openGoal();
    await expect(goalPop).toContainText("manually enter /goal clear");
    await expect(goalPop.getByRole("button", { name: "Clear goal", exact: true })).toBeEnabled();
    await page.screenshot({ path: join(screenshots, "controls-ui-mobile-unconfirmed.png") });
    await page.setViewportSize({ width: 1440, height: 1000 });
    await openGoal();
    await goalPop.getByRole("button", { name: "Clear goal", exact: true }).click();
    assert.notEqual(goalWrites.at(-1).method, "DELETE");
    await goalPop.getByRole("button", { name: "Keep goal", exact: true }).click();
    await expect(goalPop.locator(".goal-confirm")).toHaveCount(0);
    await goalPop.getByRole("button", { name: "Clear goal", exact: true }).click();
    await goalPop.getByRole("button", { name: "Confirm clear goal", exact: true }).click();
    await expect(strip.getByRole("button", { name: "Set goal", exact: true })).toBeVisible();
    const promptCount = prompts.length;
    await prompt.fill("/goal finish the release");
    await prompt.press("Enter");
    await expect(goalSegment()).toContainText("finish the release");
    assert.equal(prompts.length, promptCount);
    await prompt.fill("/goal clear");
    await prompt.press("Enter");
    await expect(page.locator(".goal-confirm")).toBeVisible();
    await expect(prompt).toHaveValue("/goal clear");
    failGoal = true;
    await page.getByRole("button", { name: "Confirm clear goal", exact: true }).click();
    await expect(feedback).toHaveText("Goal transport failed");
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
    await prompt.fill("");
    setState({ harness: { ...state.harness, capabilities: { ...state.harness.capabilities, goal: { pause: false, budget: false } } }, goal: { objective: "Unbudgeted goal", status: "active", tokensUsed: null, timeUsedSeconds: null, tokenBudget: null } });
    await openGoal();
    await expect(goalPop.getByRole("button", { name: "Pause goal", exact: true })).toHaveCount(0);
    await expect(goalPop).toContainText("Token usage unavailable");
    await goalPop.getByRole("button", { name: "Edit goal", exact: true }).click();
    await expect(editor.getByLabel("Token budget (optional)")).toHaveCount(0);
    await editor.getByRole("button", { name: "Cancel", exact: true }).click();

    await page.setViewportSize({ width: 390, height: 844 });
    for (const [status, label] of [["stale", "Stale"], ["done", "Done"], ["blocked", "Blocked"]]) {
      setState({ goal: { ...state.goal, status, reason: "Waiting for owner action", requiredAction: "Owner: provision environment" } });
      await openGoal();
      await expect(goalPop.locator(".goal-status")).toContainText(label);
    }
    await expect(goalPop).toContainText("Waiting for owner action");
    await page.screenshot({ path: join(screenshots, "controls-ui-mobile-blocked.png") });
    await page.keyboard.press("Escape");
    await prompt.fill("touch queued");
    await page.getByRole("button", { name: "Queue message", exact: true }).click();
    await expect(strip.getByRole("button", { name: /Queue/ })).toContainText("1");
    // Phone: files and tasks share one full-screen slot and stay clear of the bottom bar.
    await phoneNav(page, "Files");
    await expect(page.locator("#files")).toBeVisible();
    await phoneNav(page, "Tasks");
    await expect(page.locator("#tasks")).toBeVisible();
    await expect(page.locator("#files")).toBeHidden();
    assert.ok(await page.evaluate(() => document.querySelector("#tasks .tlist").getBoundingClientRect().bottom <= document.querySelector("#tasks").getBoundingClientRect().bottom + 1), "The task panel fills the area under the top bar");
    await page.screenshot({ path: join(screenshots, "controls-ui-mobile-tasks.png") });
    await phoneNav(page, "Files");
    await expect(page.locator("#tasks")).toBeHidden();
    await expect(page.locator("#files")).toBeVisible();
    await page.getByRole("button", { name: "Close file browser" }).click();
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "No mobile horizontal overflow");
    setState({ harness: { ...state.harness, capabilities: { steer: false, backgroundTasks: false, goal: null } } });
    await expect(page.getByRole("button", { name: /^Background tasks/ })).toHaveCount(0);
    await expect(strip.getByRole("button", { name: /^Goal|Set goal/ })).toHaveCount(0);
    await prompt.fill("/goal unsupported");
    await prompt.press("Enter");
    await expect(feedback).toContainText("does not support goals");
    assert.equal(prompts.at(-1).prompt, "touch queued");
    assert.equal(await page.evaluate(() => window.controlsXSS), undefined);
    assert.deepEqual(await page.evaluate(() => window.__violations.filter((directive) => directive !== "script-src-attr")), []);
    // The only console errors are the browser logging this fixture's six deliberate 503 responses.
    assert.deepEqual(errors.filter((message) => !/status of 503/.test(message)), []);
    assert.equal(errors.length, 6);
    console.log("React UI runtime controls mock checks passed (tasks / queue / goal, 1440px and 390px).");
  } finally {
    await context.close();
    for (const client of wss.clients) client.terminate();
    await new Promise((resolve) => wss.close(resolve));
    await new Promise((resolve) => server.close(resolve));
  }
}
