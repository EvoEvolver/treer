#!/usr/bin/env node
// Real-browser acceptance of the launcher's background-task retention and "Clear finished tasks" (#184), driven through
// the Console's embedded Agent view against a REAL launcher-backed Claude-harness fixture agent (fake Claude, no real
// credentials). It exists because the Tasks panel only appears for the Claude harness (#98 rule: real-browser acceptance
// before a browser-path change ships).
//
// Fixture contract (release owns it): one DISPOSABLE agent running
//   CLAUDE_BIN=<launcher>/tests/fake-claude.mjs AGENT_UI_CWD=<scratch dir> node <launcher>/scripts/treer-agent.mjs --harness claude
// The prompt "many-tasks" makes the fake report 105 completed tasks and one running task ("still-running"). With #184 the
// launcher keeps the running task plus the 20 newest finished ones (21 records); a build without #184 keeps 101, so this
// script FAILS on it by design. The script only sends that one prompt and uses "Clear finished tasks" on this agent;
// both are idempotent (re-running re-reports the same task ids).
//
// Inputs (environment only; nothing from argv):
//   ACCEPT_CONSOLE_URL, ACCEPT_WORKSPACE_PATH (/orgs/<org>/workspaces/<ws>), ACCEPT_AGENT_ID (the Claude fixture agent),
//   ACCEPT_STORAGE_STATE (Playwright storage state, read only, never printed or copied),
//   ACCEPT_ALLOWED_HOST_SUFFIXES (REQUIRED, canary hosts; entries with fewer than three labels are refused and the Console
//   host must be covered; any other request is aborted and fails the run), ACCEPT_IGNORED_BLOCKED_HOSTS (optional, counted),
//   ACCEPT_ENGINES (default chromium,firefox,webkit), ACCEPT_REPORT_FILE (default ./console-tasks-acceptance-report.json),
//   ACCEPT_TIMEOUT_MS (default 30000), ACCEPT_LOCALE (default en-US), ACCEPT_EXPECT_FINISHED (default 20),
//   ACCEPT_ALLOW_HTTP=1 (local stand-in only).
// Report: per step pass/fail and the observed counts; never cookies, storage state, query strings, fragments or message
// bodies. No traces, videos, HARs or screenshots. Exit 0 only if every engine passes every step.
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { chromium, firefox, webkit, expect } from "@playwright/test";

const selfHash = createHash("sha256").update(readFileSync(new URL(import.meta.url))).digest("hex");
const env = process.env;
for (const name of ["ACCEPT_CONSOLE_URL", "ACCEPT_WORKSPACE_PATH", "ACCEPT_AGENT_ID", "ACCEPT_STORAGE_STATE"]) if (!env[name]) { console.error(`missing ${name}`); process.exit(2); }
const consoleOrigin = new URL(env.ACCEPT_CONSOLE_URL).origin;
if (!/^\/orgs\/[^/]+\/workspaces\/[^/]+$/.test(env.ACCEPT_WORKSPACE_PATH)) { console.error("ACCEPT_WORKSPACE_PATH must look like /orgs/<org>/workspaces/<workspace>"); process.exit(2); }
if (!/^[a-zA-Z0-9_-]{1,128}$/.test(env.ACCEPT_AGENT_ID)) { console.error("ACCEPT_AGENT_ID is not a valid workload id"); process.exit(2); }
if (!existsSync(env.ACCEPT_STORAGE_STATE)) { console.error("ACCEPT_STORAGE_STATE file not found"); process.exit(2); }
if (!env.ACCEPT_ALLOWED_HOST_SUFFIXES) { console.error("ACCEPT_ALLOWED_HOST_SUFFIXES is required (comma list of canary hosts or canary-only suffixes)"); process.exit(2); }
const isIp = (value) => /^\d+\.\d+\.\d+\.\d+$/.test(value) || value === "localhost";
const suffixes = env.ACCEPT_ALLOWED_HOST_SUFFIXES.split(",").map((value) => value.trim().toLowerCase()).filter(Boolean);
for (const suffix of suffixes) if (!isIp(suffix) && suffix.split(".").length < 3) { console.error(`allowed host entry "${suffix}" is too broad; name canary hosts (at least three labels)`); process.exit(2); }
const hostAllowed = (host) => suffixes.some((suffix) => host === suffix || host.endsWith(`.${suffix}`));
if (!hostAllowed(new URL(consoleOrigin).hostname)) { console.error("the Console host is not covered by ACCEPT_ALLOWED_HOST_SUFFIXES"); process.exit(2); }
const ignoredBlocked = (env.ACCEPT_IGNORED_BLOCKED_HOSTS || "").split(",").map((value) => value.trim().toLowerCase()).filter(Boolean);
const timeout = Number(env.ACCEPT_TIMEOUT_MS || 30000);
const locale = env.ACCEPT_LOCALE || "en-US";
const expectFinished = Number(env.ACCEPT_EXPECT_FINISHED || 20);
const reportFile = env.ACCEPT_REPORT_FILE || "console-tasks-acceptance-report.json";
const engines = { chromium, firefox, webkit };
const wanted = (env.ACCEPT_ENGINES || "chromium,firefox,webkit").split(",").map((name) => name.trim()).filter(Boolean);
for (const name of wanted) if (!engines[name]) { console.error(`unknown engine ${name}`); process.exit(2); }
const agentUrl = consoleOrigin + env.ACCEPT_WORKSPACE_PATH + "/workloads/" + encodeURIComponent(env.ACCEPT_AGENT_ID);

const clean = (value) => String(value).replace(/[?#].*$/, "").replace(/iec_v\d+_[0-9a-f]+/g, "iec_***").replace(/[A-Za-z0-9_-]{32,}/g, (run) => run.length > 40 && !/^[a-z-]+$/.test(run) ? "***" : run).slice(0, 300);

async function runEngine(name) {
  const steps = [], problems = [], observed = {}, ignoredBlockedCount = {};
  const aborted = { count: 0 };
  const step = async (title, fn) => {
    if (env.ACCEPT_TRACE === "1") console.error(`[${name}] > ${title}`);
    const deadline = new Promise((_, reject) => setTimeout(() => reject(new Error(`step exceeded its ${timeout * 4} ms deadline`)), timeout * 4).unref());
    try { const detail = await Promise.race([fn(), deadline]); steps.push({ step: title, status: "pass", ...(detail ? { detail } : {}) }); return true; }
    catch (error) { steps.push({ step: title, status: "fail", detail: clean(error instanceof Error ? error.message.split("\n")[0] : String(error)) }); return false; }
  };
  const browser = await engines[name].launch();
  try {
    const context = await browser.newContext({ locale, storageState: env.ACCEPT_STORAGE_STATE, viewport: { width: 1360, height: 900 } });
    await context.route("**/*", (route) => {
      const url = new URL(route.request().url());
      if (["about:", "data:", "blob:"].includes(url.protocol) || hostAllowed(url.hostname)) return route.continue();
      if (ignoredBlocked.includes(url.hostname)) ignoredBlockedCount[url.hostname] = (ignoredBlockedCount[url.hostname] || 0) + 1;
      else problems.push(`blocked request outside the allowed hosts: ${clean(route.request().url())}`);
      return route.abort();
    });
    const page = await context.newPage();
    page.on("pageerror", (error) => problems.push(`pageerror: ${clean(error.message)}`));
    page.on("console", (message) => {
      if (ignoredBlocked.some((host) => message.text().includes(host) || (message.location().url || "").includes(host))) return;
      if (message.type() === "error") problems.push(`console: ${clean(message.text())} @ ${clean(message.location().url || "")}`);
    });
    page.on("requestfailed", (request) => {
      const reason = request.failure()?.errorText || "";
      if (ignoredBlocked.includes(new URL(request.url()).hostname)) return;
      if (/ABORTED|cancel/i.test(reason) && ["fetch", "xhr"].includes(request.resourceType())) { aborted.count++; return; }
      problems.push(`request failed: ${request.method()} ${clean(request.url())} (${clean(reason)})`);
    });
    page.on("response", (response) => { if (response.status() >= 400) problems.push(`HTTP ${response.status()} ${response.request().method()} ${clean(response.url())}`); });

    const frame = page.frameLocator('iframe[title$=" interface"]');
    const panel = frame.locator("#tasks");
    const tasks = panel.locator(".task");
    const trigger = panel.getByRole("button", { name: "Clear finished tasks", exact: true });
    const confirm = panel.getByRole("group", { name: "Clear finished tasks" });
    const closeButton = panel.getByRole("button", { name: "Close background tasks" });

    await step("Console opens the Claude fixture agent and its embedded UI is usable", async () => {
      await page.goto(agentUrl, { waitUntil: "domcontentloaded", timeout });
      await expect(page.locator('iframe[title$=" interface"]')).toBeVisible({ timeout });
      await expect(frame.getByRole("textbox", { name: /Message/ })).toBeVisible({ timeout });
      await expect(frame.getByRole("button", { name: /^Background tasks/ })).toBeVisible({ timeout }); // Claude harness: the panel exists
    });
    await step("sending 'many-tasks' is accepted and the launcher reports a running task", async () => {
      const box = frame.getByRole("textbox", { name: /Message/ });
      await box.fill("many-tasks");
      await expect(frame.getByRole("button", { name: "Send message", exact: true })).toBeEnabled({ timeout });
      const accepted = page.waitForResponse((response) => response.request().method() === "POST" && /\/api\/prompt$/.test(new URL(response.url()).pathname), { timeout });
      await box.press("Enter");
      const response = await accepted;
      if (response.status() !== 200) throw new Error(`the launcher answered the prompt with HTTP ${response.status()}`);
      await expect(frame.getByRole("button", { name: /^Background tasks, \d+ running/ })).toContainText("1 running", { timeout });
    });
    await step(`the panel keeps ${expectFinished} finished tasks plus the running one (retention)`, async () => {
      await frame.getByRole("button", { name: /^Background tasks/ }).click();
      await expect(panel).toBeVisible({ timeout });
      // Wait until the 105 notifications have been folded, then require the exact bound.
      await expect.poll(async () => await tasks.count(), { timeout }).toBe(expectFinished + 1);
      const running = await panel.locator(".task-running").count();
      if (running !== 1) throw new Error(`expected exactly one running task, found ${running}`);
      const completed = await panel.locator(".task-completed").count();
      observed.records_before = expectFinished + 1; observed.running_before = running; observed.completed_before = completed;
      return { records: expectFinished + 1, running, completed };
    });
    await step("Clear finished tasks: 44 px trigger, confirmation text, focus on the destructive button", async () => {
      await expect(trigger).toBeVisible({ timeout });
      const box = await trigger.boundingBox();
      if (!box || box.height < 44 || box.width < 44) throw new Error(`the trigger is ${box?.width}x${box?.height}`);
      await trigger.click();
      await expect(confirm).toContainText(`Remove ${expectFinished} finished task records`);
      await expect(confirm).toContainText("Running tasks and their saved output are not affected");
      await expect(confirm.getByRole("button", { name: "Clear", exact: true })).toBeFocused();
      for (const control of await confirm.getByRole("button").all()) { const b = await control.boundingBox(); if (!b || b.height < 44) throw new Error("a confirmation control is under 44 px tall"); }
    });
    await step("Keep and Escape cancel without clearing and return focus to the trigger", async () => {
      // Keyboard traversal from Clear to Keep. Safari/WebKit on macOS skips buttons on a plain Tab (its default; Option+Tab
      // visits every control), so try Tab, then Alt+Tab, then a programmatic focus, and record which one worked.
      const keep = confirm.getByRole("button", { name: "Keep" });
      const clearButton = confirm.getByRole("button", { name: "Clear", exact: true });
      let focusMove = "Tab";
      await page.keyboard.press("Tab");
      if (!await keep.evaluate((node) => node === document.activeElement).catch(() => false)) {
        await clearButton.focus(); focusMove = "Alt+Tab";
        await page.keyboard.press("Alt+Tab");
        if (!await keep.evaluate((node) => node === document.activeElement).catch(() => false)) { await keep.focus(); focusMove = "programmatic focus (this engine does not Tab to buttons)"; }
      }
      observed.keep_focus_move = focusMove;
      await expect(keep).toBeFocused();
      await page.keyboard.press("Enter");
      await expect(confirm).toHaveCount(0);
      await expect(trigger).toBeFocused();
      await expect(tasks).toHaveCount(expectFinished + 1);
      await page.keyboard.press("Enter");
      await expect(confirm).toBeVisible();
      await page.keyboard.press("Escape");
      await expect(confirm).toHaveCount(0);
      await expect(panel).toBeVisible(); // Escape cancelled only the confirmation
      await expect(trigger).toBeFocused();
      await expect(tasks).toHaveCount(expectFinished + 1);
      return { focus_move_to_keep: focusMove };
    });
    await step("confirming clears only the finished records, keeps the running one and focuses Close", async () => {
      await page.keyboard.press("Enter");
      await expect(confirm).toBeVisible();
      await confirm.getByRole("button", { name: "Clear", exact: true }).press("Enter");
      await expect(tasks).toHaveCount(1, { timeout });
      await expect(panel.locator(".task-running")).toHaveCount(1);
      await expect(panel.locator(".task-completed")).toHaveCount(0);
      await expect(trigger).toHaveCount(0);
      await expect(closeButton).toBeFocused();
      await expect(panel.getByRole("alert")).toHaveCount(0);
      observed.records_after = 1;
      return { records_after: 1, running_after: 1 };
    });
    await step("the launcher state agrees after a reload (records were removed, not hidden)", async () => {
      await page.reload({ waitUntil: "domcontentloaded", timeout });
      await expect(frame.getByRole("textbox", { name: /Message/ })).toBeVisible({ timeout });
      await frame.getByRole("button", { name: /^Background tasks/ }).click();
      await expect(tasks).toHaveCount(1, { timeout });
      await expect(trigger).toHaveCount(0);
    });
    await context.close();
  } catch (error) {
    steps.push({ step: "run", status: "fail", detail: clean(error instanceof Error ? error.message.split("\n")[0] : String(error)) });
  } finally { await Promise.race([browser.close().catch(() => {}), new Promise((resolve) => setTimeout(resolve, 15000).unref())]); }
  if (!problems.length) steps.push({ step: "no console errors, failed requests, HTTP errors or blocked requests", status: "pass" });
  else steps.push({ step: "no console errors, failed requests, HTTP errors or blocked requests", status: "fail", detail: problems.slice(0, 20) });
  return { engine: name, passed: steps.every((entry) => entry.status === "pass"), steps, observed, ignored_aborted_fetches: aborted.count, ignored_blocked_requests: ignoredBlockedCount };
}

const results = [];
for (const name of wanted) results.push(await runEngine(name));
const report = { script_sha256: selfHash, console_origin: consoleOrigin, workspace_path: env.ACCEPT_WORKSPACE_PATH, agent_id: env.ACCEPT_AGENT_ID, expect_finished: expectFinished, passed: results.every((entry) => entry.passed), results };
writeFileSync(reportFile, JSON.stringify(report, null, 2) + "\n", { mode: 0o600 });
console.log(`script sha256 ${selfHash}`);
for (const result of results) {
  console.log(`${result.passed ? "PASS" : "FAIL"} ${result.engine}`);
  for (const entry of result.steps) console.log(`  ${entry.status.toUpperCase()} ${entry.step}${entry.status === "fail" ? `: ${JSON.stringify(entry.detail)}` : ""}`);
}
console.log(report.passed ? "TASKS ACCEPTANCE PASSED" : "TASKS ACCEPTANCE FAILED");
process.exit(report.passed ? 0 : 1);
