import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdtemp, mkdir, writeFile, readFile, rm, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { chromium, expect } from "@playwright/test";
import { hermeticEnv, localFetch as fetch } from "./hermetic.mjs";
import { testRuntimeControlsUi } from "./browser-controls-ui.mjs";
import { testImagesUi } from "./browser-images-ui.mjs";
import { testFilesDownloadUi } from "./browser-files-download-ui.mjs";
import { testMobileComposer } from "./browser-mobile-composer.mjs";
import { testHistoryPages } from "./browser-history-pages.mjs";
import { fakePrivateState } from "./fake-private-state.mjs";
import { backend, createSteps, spawnLauncher, supports } from "./launcher-backend.mjs";

// The React UI (public/ui.html) against the real launcher server and a fake Codex: no inference, no Treer mutations.
// Usage: node tests/browser-ui.mjs [--page index.html]
const app = resolve(import.meta.dirname, "..");
const pageArgIndex = process.argv.indexOf("--page");
const page_ = pageArgIndex > 0 ? process.argv[pageArgIndex + 1] : "index.html";
const screenshots = process.env.UI_SCREENSHOTS || process.env.CODEX_UI_SCREENSHOTS || await mkdtemp(join(tmpdir(), "ui-shots-"));
await mkdir(screenshots, { recursive: true });
const servers = [];
const unsupported = (what) => Object.assign(new Error(`${what} is not implemented by the ${backend} launcher`), { unsupported: true });
async function startServer(name, extra, harness = "codex") {
  if (!supports(harness)) throw unsupported(`The ${harness} harness`);
  const cwd = await mkdtemp(join(tmpdir(), `ui-browser-${name}-`));
  const privateRoot = await realpath(await mkdtemp(join(tmpdir(), `ui-images-${name}-`)));
  const controller = await fakePrivateState("ag_browser_ui");
  await mkdir(join(cwd, "src/deep"), { recursive: true });
  await mkdir(join(cwd, "empty"));
  await writeFile(join(cwd, "src/greeting.ts"), "export const greeting = 'Hello, Treer';\n// <script>window.fileXSS = true</script>\n");
  await writeFile(join(cwd, "src/deep/inner.txt"), "inner file\n");
  await writeFile(join(cwd, "README.md"), "# Workspace\nSmall tools, thoughtful interfaces.\n");
  await writeFile(join(cwd, "special # 100%.txt"), "literal percent and hash filename\n");
  await writeFile(join(cwd, "large.txt"), "x".repeat(65537));
  // The unique marker identifies exactly the processes this test started; they are stopped through their own handles only.
  const common = { UI_DEV_FIXTURE: randomUUID(), TREER_AGENT_PRIVATE_STATE_DIR: privateRoot, TREER_AGENT_ID: "ag_browser_ui", TREER_AGENT_SERVER_URL: controller.url, TREER_WORKLOAD_CREDENTIAL: "fixture-only", AGENT_UI_MESSAGE_WATCH: "0", AGENT_UI_INSTANCE_ID: "browser-ui", CODEX_UI_PUBLIC_DIR: join(app, "public"), CODEX_UI_PORT: "0" };
  const env = hermeticEnv(harness === "claude"
    ? { ...common, AGENT_UI_HARNESS: "claude", AGENT_UI_CWD: cwd, AGENT_UI_PORT: "0", AGENT_UI_PORT_FILE: join(cwd, "port"), AGENT_UI_PUBLIC_DIR: join(app, "public"), CLAUDE_BIN: join(app, "tests/fake-claude.mjs"), ...extra }
    : { ...common, CODEX_UI_CWD: cwd, CODEX_UI_PORT_FILE: join(cwd, "port"), CODEX_BIN: join(app, "tests/fake-codex.mjs"), FAKE_CODEX_UX: "1", ...extra });
  const server = spawnLauncher(app, env);
  let logs = "";
  server.stderr.on("data", (chunk) => { logs += chunk; });
  server.stdout.on("data", (chunk) => { logs += chunk; });
  servers.push({ server, cwd, privateRoot, controller });
  let base;
  for (let attempt = 0; attempt < 100; attempt++) {
    if (server.exitCode !== null) throw new Error(logs);
    try {
      base = `http://127.0.0.1:${Number(await readFile(join(cwd, "port"), "utf8"))}`;
      if ((await fetch(base + "/api/health")).ok) break;
    } catch { /* wait for the isolated listener */ }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  assert.ok(base, logs);
  return base;
}
// Phones have no bottom bar: Files, Tasks, Appearance and Theme live in the Menu button in the header.
async function phoneNav(page, item) {
  await page.getByRole("button", { name: "Menu", exact: true }).click();
  await page.getByRole("menuitem", { name: item, exact: true }).click();
}
async function assertTouchTargets(page, selector, label) {
  const controls = page.locator(selector).filter({ visible: true });
  assert.ok(await controls.count() > 0, `${label}: controls must exist`);
  for (const control of await controls.all()) {
    const bounds = await control.boundingBox();
    assert.ok(bounds && bounds.width >= 44 && bounds.height >= 44,
      `${label}: ${await control.getAttribute("class")} is ${bounds?.width}x${bounds?.height}, expected >=44x44`);
  }
}
let browser;
let base, url, context, page, errors, live, claude, done, imageBase, idle; // shared by the steps below
const steps = createSteps();
const { step } = steps;
const recover = async () => { await page.setViewportSize({ width: 1440, height: 900 }); await page.goto(url("?look=b")); };
try {
  base = await startServer("rich", { FAKE_CODEX_RICH: "1", FAKE_CODEX_MANY: "80", FAKE_CODEX_MODELS: "1", FAKE_CODEX_GOAL_ACTIVE: "1", FAKE_CODEX_COMPLETE_TURNS: "1", FAKE_CODEX_FILE_LINKS: "1" });
  url = (query = "") => `${base}/${page_}${query}`;

  await step('page headers and CSP', async () => {
  // Headers: the React page gets the strict policy, the legacy page is unchanged.
  const csp = (await fetch(url())).headers.get("content-security-policy") ?? "";
  const asset = await fetch(url().replace(/index\.html$/, "ui.js"));
  const pageHeaders = (await fetch(url())).headers;
  for (const feature of ["camera", "microphone", "geolocation", "payment", "usb", "display-capture", "clipboard-read", "bluetooth", "hid", "serial", "midi", "accelerometer", "gyroscope", "magnetometer", "screen-wake-lock"]) assert.ok(pageHeaders.get("permissions-policy")?.includes(`${feature}=()`));
  assert.equal(asset.headers.get("x-content-type-options"), "nosniff");
  assert.equal(asset.headers.get("referrer-policy"), "no-referrer");
  assert.match(csp, /script-src 'self'/);
  assert.match(csp, /style-src 'self'(?:;|$)/);
  assert.doesNotMatch(csp, /unsafe-inline|unsafe-eval/);
  for (const directive of ["object-src 'none'", "base-uri 'self'", "form-action 'none'"]) assert.ok(csp.includes(directive), `CSP lacks ${directive}`);
  assert.match((await fetch(base + "/")).headers.get("content-security-policy") ?? "", /style-src 'self'(?:;|$)/, "the root serves the same page under the same policy");

  }, recover);
  browser = await chromium.launch({ headless: true });
  context = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: "light", extraHTTPHeaders: { "x-treer-interface-source": "proxy-ui", "x-treer-agent-id": "ag_browser_ui", "x-treer-interface-instance": "browser-ui" } });
  await context.addInitScript(() => { window.__violations = []; document.addEventListener("securitypolicyviolation", (event) => window.__violations.push(`${event.violatedDirective} ${event.blockedURI} ${event.sample} ${event.sourceFile}:${event.lineNumber}`)); });
  page = await context.newPage();
  errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (message.type() !== "error") return;
    const expectedFileFailures = new Map([[base + "/v1/files?path=missing.txt", 404], [base + "/v1/files?path=..%2Fescape.txt", 403]]);
    const expectedStatus = expectedFileFailures.get(message.location().url);
    if (expectedStatus && message.text().includes(`status of ${expectedStatus}`)) return;
    errors.push(message.text() + (message.location().url ? ` (${message.location().url})` : ""));
  });

  await step('first paint', async () => {
  // First paint: boot.js has set look and theme, React renders the real conversation from the real API.
  await page.goto(url());
  await expect(page.locator("html")).toHaveAttribute("data-look", "b");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await expect(page.locator(".app")).toBeVisible();
  await expect(page.locator(".chat .msg.asst").filter({ hasText: "file browser is ready" })).toHaveCount(1);
  await expect(page.locator("h1")).not.toBeEmpty();
  assert.equal(await page.evaluate(() => document.querySelectorAll("script:not([src])").length), 0, "no inline scripts");
  const inlineStyles = await page.evaluate(() => [...document.querySelectorAll("[style]")].map((e) => `${e.tagName.toLowerCase()}: ${e.getAttribute("style")}`).filter((style) => !/^textarea: height:\s*[\d.]+px;?$|^html: --dock-h:\s*[\d.]+px;?$/.test(style)));
  assert.deepEqual(inlineStyles, [], "only CSSOM-set composer height and --dock-h exist; no markup styles");

  }, recover);
  await step('conversation body', async () => {
  // Conversation body: markdown through the one sanitized sink, collapsed activity, diffs, plans, failures.
  const chat = page.locator(".chat");
  await expect(chat.locator(".msg.asst strong", { hasText: "src/" })).toHaveCount(1);
  await expect(chat.locator(".markdown h2", { hasText: "Result" })).toHaveCount(1);
  await expect(chat.locator(".markdown table td")).toHaveCount(2);
  await expect(chat.locator(".markdown pre code")).toContainText("const a = 1;");
  await expect(chat.locator(".markdown a", { hasText: "the docs" })).toHaveAttribute("rel", "noopener noreferrer");
  await expect(chat.locator(".markdown a", { hasText: "the docs" })).toHaveAttribute("target", "_blank");
  assert.deepEqual(await page.evaluate(() => ({ xss: window.xss ?? null, scripts: document.querySelectorAll(".chat script").length, handlers: document.querySelectorAll(".chat [onerror], .chat [onload]").length, js: document.querySelectorAll('.chat a[href^="javascript"]').length, remote: document.querySelectorAll('.chat img[src^="http"]').length })), { xss: null, scripts: 0, handlers: 0, js: 0, remote: 0 }, "hostile markdown must stay inert");
  assert.deepEqual(await page.evaluate(() => ({ schemes: [...document.querySelectorAll(".chat a[href]")].map((a) => a.getAttribute("href")).filter((h) => (/^[a-z][a-z0-9+.-]*:/i.test(h) && !/^(https?:|mailto:)/i.test(h)) || h.startsWith("//") || h.includes("\\")), forms: document.querySelectorAll(".chat form, .chat input, .chat button.x, .chat [name]").length })), { schemes: [], forms: 0 }, "no foreign-scheme, protocol-relative or backslash links survive; no forms");
  const groups = chat.locator(".grp");
  await expect(groups).toHaveCount(2);
  await expect(groups.first().locator(".sum")).toHaveAttribute("aria-expanded", "false");
  await expect(groups.first().locator(".t")).toHaveText("Activity");
  await expect(groups.first()).toContainText("1 file change");
  await groups.first().locator(".sum").click();
  await expect(groups.first().locator(".sum")).toHaveAttribute("aria-expanded", "true");
  await expect(groups.first().locator(".diff")).toHaveCount(2);
  await expect(groups.first().locator(".diff").first().locator(".dl.add")).toContainText("Hello, Treer");
  await expect(groups.first().locator(".diff").first().locator(".dl.del")).toContainText("Hello");
  await expect(groups.last()).toHaveAttribute("data-warn", "true");
  await expect(groups.last()).toContainText("1 failed");
  await groups.last().locator(".sum").click();
  await groups.last().locator(".tool-btn", { hasText: "durable-queue" }).click();
  await expect(groups.last().locator(".tool-detail")).toContainText("not ok 1 - fixture path");
  await groups.last().locator(".tool-btn", { hasText: "Reasoning" }).click().catch(() => undefined);
  await expect(chat.locator(".plan li")).toHaveCount(3);
  await expect(chat.locator(".plan li.done")).toHaveCount(1);
  await expect(chat.locator(".plan li.cur")).toHaveCount(1);
  assert.ok(await page.evaluate(() => document.documentElement.scrollHeight <= innerHeight && document.documentElement.scrollTop === 0), "the page itself never scrolls; only the thread does");
  await page.screenshot({ path: join(screenshots, "conversation-b-light.png") });

  }, recover);
  await step('touch hit areas', async () => {
  // Actual touch hit areas in all looks/themes, before paging consumes the earlier-messages button.
  for (const look of ["a", "b", "c"]) {
    for (const theme of ["light", "dark"]) {
      await page.setViewportSize({ width: 390, height: 844 });
      await page.goto(url(`?look=${look}&theme=${theme}`));
      await expect(page.locator(".older .btn")).toBeVisible();
      for (const selector of [".nav-btn", ".send", ".stopb", "button.model", ".seg", ".older .btn", ".sum"]) {
        await assertTouchTargets(page, selector, `${look}/${theme} compact`);
      }
      await page.getByRole("textbox", { name: /Message/ }).fill("Touch target draft");
      await assertTouchTargets(page, ".ctl button", `${look}/${theme} expanded`);
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `${look}/${theme}: no page overflow`);
      assert.ok(await page.locator(".ctl").evaluate((el) => el.scrollWidth <= el.clientWidth), `${look}/${theme}: no composer overflow`);
      await page.screenshot({ path: join(screenshots, `touch-390-${look}-${theme}.png`) });
    }
  }
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(url());

  }, recover);
  await step('paging older messages', async () => {
  if (backend === "rust") {
    const before=await page.locator(".chat .msg.user").first().textContent();
    await page.getByRole("button",{name:"Load earlier messages",exact:true}).click();
    await expect(page.getByText("Viewing an earlier page",{exact:true})).toBeVisible();
    assert.ok(await page.locator(".chat .msg.user").count()<=50);
    assert.notEqual(await page.locator(".chat .msg.user").first().textContent(),before);
    await page.getByRole("button",{name:"Back to latest messages",exact:true}).click();
    await expect(page.getByText("Viewing an earlier page",{exact:true})).toHaveCount(0);
    return;
  }
  // Paging: 80 older turns exist; "Load earlier messages" keeps the reader's place and the window grows.
  await expect(page.locator(".older .btn")).toBeVisible();
  const before = await page.locator(".chat .msg.user").count();
  const probe = page.locator(".chat .msg.user").first();
  const probeText = await probe.textContent();
  const probeTop = (await probe.boundingBox()).y;
  const thread = page.locator("#thread");
  await thread.evaluate((element) => { element.scrollTop = 0; });
  await expect(page.locator("#jump")).toBeVisible();
  await page.locator(".older .btn").click();
  await expect.poll(() => page.locator(".chat .msg.user").count()).toBeGreaterThan(before);
  const same = page.locator(".chat .msg.user", { hasText: probeText.replace(/^You/, "").trim() }).first();
  const scrollTopAfter = await thread.evaluate((element) => element.scrollTop);
  assert.ok(scrollTopAfter > 100, "scroll position must be anchored, not reset to the top");
  await same.scrollIntoViewIfNeeded();
  await page.locator("#jump").click();
  await expect.poll(() => thread.evaluate((element) => element.scrollHeight - element.scrollTop - element.clientHeight)).toBeLessThan(40);
  await expect(page.locator("#jump")).toHaveCount(0);

  }, recover);
  await step('looks and themes', async () => {
  // 3 looks x light/dark, persisted look, whitelisted ?look, theme sources.
  const menuButton = { click: () => page.locator('button[aria-controls="appearance"]:visible').first().click() };
  for (const look of ["a", "b", "c"]) {
    await menuButton.click();
    await page.getByRole("radio", { name: new RegExp({ a: "Quiet Editor", b: "Warm Paper", c: "Terminal Contrast" }[look]) }).click();
    await expect(page.locator("html")).toHaveAttribute("data-look", look);
    assert.equal(await page.evaluate(() => localStorage.getItem("treer-launcher-look")), look);
    for (const theme of ["light", "dark"]) {
      await page.getByRole("radio", { name: theme === "light" ? "Light" : "Dark", exact: true }).click();
      await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
      await page.screenshot({ path: join(screenshots, `desktop-${look}-${theme}.png`) });
    }
    await page.keyboard.press("Escape");
    await expect(page.locator("#appearance")).toHaveCount(0);
  }
  await page.goto(url("?look=zzz"));
  await expect(page.locator("html")).toHaveAttribute("data-look", "c", "an invalid ?look falls back to the stored look");
  await page.goto(url("?look=a&theme=dark"));
  await expect(page.locator("html")).toHaveAttribute("data-look", "a");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.evaluate(() => localStorage.clear());
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto(url());
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark", "system preference");
  await page.emulateMedia({ colorScheme: "light" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light", "system changes are followed");
  await page.evaluate(() => window.postMessage({ type: "treer:embed-theme", theme: "dark", look: "a" }, location.origin));
  await page.waitForTimeout(50);
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light", "standalone pages ignore self/other-window embed messages");
  await page.goto(`${base}/api/health`); // a same-origin page without the UI policy; setContent would otherwise inherit it
  await page.setContent(`<iframe title="Agent" src="${url("?theme=light")}" style="width:100%;height:700px"></iframe>`);
  const frame = page.frameLocator("iframe");
  await expect(frame.locator("html")).toHaveAttribute("data-theme", "light");
  await page.evaluate(() => document.querySelector("iframe").contentWindow.postMessage({ type: "treer:embed-theme", theme: "dark" }, "*"));
  await expect(frame.locator("html")).toHaveAttribute("data-theme", "dark", "parent message");
  await page.evaluate(() => document.querySelector("iframe").contentWindow.postMessage({ type: "treer:embed-theme", theme: "dark", look: "c", lang: "zh" }, "*"));
  await expect(frame.locator("html")).toHaveAttribute("data-look", "c", "the parent message may set the look");
  await page.evaluate(() => document.querySelector("iframe").contentWindow.postMessage({ type: "treer:embed-theme", theme: "dark", look: "<x>" }, "*"));
  await expect(frame.locator("html")).toHaveAttribute("data-look", "c", "an unknown look in the message is ignored");
  assert.equal(await frame.locator("html").evaluate(() => localStorage.getItem("treer-launcher-look")), null, "a console-sent look is not persisted");

  }, recover);
  await step('send a message', async () => {
  // Send a message through the real API; a fake Codex answers and the WebSocket pushes the state.
  await page.goto(url("?look=b"));
  const box = page.getByRole("textbox", { name: /Message/ });
  await box.fill("hello from the react ui");
  const sent = page.waitForResponse((response) => response.url().endsWith("/api/prompt") && response.request().method() === "POST");
  await box.press("Enter");
  const response = await sent;
  assert.equal(response.status(), 200);
  assert.equal(JSON.parse(response.request().postData()).prompt, "hello from the react ui");
  await expect(box).toHaveValue("");

  }, recover);
  await step('composer, goal and slash commands', async () => {
  // Composer and status strip on the real API: goal lifecycle, plan, one popover at a time, model settings.
  await page.goto(url("?look=b"));
  const strip = page.getByRole("toolbar", { name: "Run status" });
  await expect(strip.getByRole("button", { name: /Plan/ })).toContainText("1/3");
  await strip.getByRole("button", { name: "Set goal" }).click();
  const dialog = page.getByRole("dialog", { name: "Set goal" });
  await expect(dialog).toBeVisible();
  await dialog.getByLabel("Objective").fill("Ship the React UI");
  await dialog.getByLabel("Token budget (optional)").fill("0");
  await dialog.getByRole("button", { name: "Save goal" }).click();
  await expect(dialog).toBeVisible(); // native validation (min=1) blocks the submit
  assert.equal(await dialog.getByLabel("Token budget (optional)").evaluate((input) => input.validity.valid), false);
  await dialog.getByLabel("Token budget (optional)").fill("5000");
  await dialog.getByRole("button", { name: "Save goal" }).click();
  await expect(dialog).toBeHidden();
  const goalSegment = strip.getByRole("button", { name: /^Goal/ });
  await expect(goalSegment).toContainText("Ship the React UI");
  await goalSegment.click();
  const goalPop = page.getByRole("region", { name: "Goal" });
  await expect(goalPop).toContainText("Ship the React UI");
  await expect(goalPop.getByRole("progressbar", { name: "Goal token budget" })).toBeVisible();
  await expect(goalPop).toContainText("Token limit: 5,000 · Round limit: Unlimited");
  await goalPop.getByRole("button", { name: "Edit goal" }).click();
  const editBudget = page.getByRole("dialog", { name: "Edit goal" });
  await editBudget.getByLabel("Token budget (optional)").fill("");
  await expect(editBudget.getByLabel("Token budget (optional)")).toHaveAttribute("placeholder", "Unlimited");
  await expect(editBudget).toContainText("Leave blank for unlimited");
  await editBudget.getByRole("button", { name: "Save goal" }).click(); await expect(editBudget).toBeHidden();
  await goalSegment.click();
  await expect(goalPop).toContainText("Token limit: Unlimited · Round limit: Unlimited");
  await expect(goalPop.getByRole("progressbar", { name: "Goal token budget" })).toHaveCount(0);
  await goalPop.getByRole("button", { name: "Pause goal" }).click();
  await expect(goalPop).toContainText("Paused");
  await goalPop.getByRole("button", { name: "Resume goal" }).click();
  await expect(goalPop).toContainText("Active");
  await goalPop.getByRole("button", { name: "Clear goal", exact: true }).click();
  await expect(goalPop.getByRole("group", { name: "Confirm clear goal" })).toBeVisible();
  await goalPop.getByRole("button", { name: "Keep goal" }).click();
  await expect(goalPop.getByRole("group", { name: "Confirm clear goal" })).toHaveCount(0);
  // One popover at a time; Esc closes and returns focus to the segment that opened it.
  await strip.getByRole("button", { name: /Plan/ }).click();
  await expect(page.getByRole("region", { name: "Goal" })).toHaveCount(0);
  await expect(page.locator("#pop-plan").locator("li")).toHaveCount(3);
  await page.keyboard.press("Escape");
  await expect(page.locator("#pop-plan")).toHaveCount(0);
  await expect(strip.getByRole("button", { name: /Plan/ })).toBeFocused();
  // Slash commands manage the goal instead of being sent as chat.
  const composerBox = page.getByRole("textbox", { name: /Message/ });
  await composerBox.fill("/goal clear");
  await composerBox.press("Enter");
  const confirm = page.getByRole("region", { name: "Goal" }).getByRole("group", { name: "Confirm clear goal" });
  await expect(confirm).toBeVisible();
  await confirm.getByRole("button", { name: "Confirm clear goal" }).click();
  await expect(strip.getByRole("button", { name: "Set goal" })).toBeVisible();
  await expect(composerBox).toHaveValue("");
  await composerBox.fill("/goal Second objective");
  await composerBox.press("Enter");
  await expect(strip.getByRole("button", { name: /^Goal/ })).toContainText("Second objective");
  await expect(composerBox).toHaveValue("");
  }, recover);
  await step('model and effort settings', async () => {
  // Model and effort through the settings API.
  await page.getByRole("button", { name: /^Model/ }).click();
  const modelPop = page.getByRole("region", { name: "Model and effort" });
  const settings = page.waitForRequest((request) => request.url().endsWith("/api/settings") && request.method() === "POST");

  await modelPop.locator("select").first().selectOption("gpt-b");
  assert.equal(JSON.parse((await settings).postData()).model, "gpt-b");
  await expect(page.getByRole("button", { name: /^Model GPT B/ })).toBeVisible();
  await page.keyboard.press("Escape");
  await page.screenshot({ path: join(screenshots, "composer-b-light.png") });

  }, recover);
  await step('running agent: steer, queue, interrupt', async () => {
  // A running agent: separate Steer and Queue buttons, queued messages, interrupt.
  live = await startServer("live", { FAKE_CODEX_LIVE: "1", FAKE_CODEX_MODELS: "1" });
  await page.goto(`${live}/${page_}?look=a`);
  const liveBox = page.getByRole("textbox", { name: /Message/ });
  await expect(page.getByRole("button", { name: "Stop current run" })).toBeEnabled();
  await page.setViewportSize({ width: 390, height: 844 });
  await liveBox.fill("touch draft");
  await assertTouchTargets(page, ".ctl button", "running steer/queue/stop");
  assert.ok(await page.locator(".ctl").evaluate((el) => el.scrollWidth <= el.clientWidth), "running composer has no horizontal overflow");
  await page.setViewportSize({ width: 1440, height: 900 });
  await liveBox.fill("steer me");
  const steered = page.waitForRequest((request) => request.url().endsWith("/api/prompt"));
  await page.getByRole("button", { name: "Steer current run" }).click();
  assert.equal(JSON.parse((await steered).postData()).mode, "steer");
  await expect(liveBox).toHaveValue("");
  await liveBox.fill("later please");
  const queued = page.waitForRequest((request) => request.url().endsWith("/api/prompt"));
  await page.getByRole("button", { name: "Queue", exact: true }).click();
  assert.equal(JSON.parse((await queued).postData()).mode, "queue");
  const queueSegment = page.getByRole("toolbar", { name: "Run status" }).getByRole("button", { name: /Queue/ });
  await expect(queueSegment).toContainText("1");
  await liveBox.fill("tab queues");
  const tabbed = page.waitForRequest((request) => request.url().endsWith("/api/prompt"));
  await liveBox.press("Tab");
  assert.equal(JSON.parse((await tabbed).postData()).mode, "queue");
  await expect(queueSegment).toContainText("2");
  await queueSegment.click();
  const queuePop = page.getByRole("region", { name: "Queued messages" });
  await expect(queuePop.locator(".qrow")).toHaveCount(2);
  const cancelled = page.waitForRequest((request) => request.method() === "DELETE" && request.url().includes("/api/queue/"));
  await queuePop.getByRole("button", { name: /Cancel queued message: later please/ }).click();
  await cancelled;
  await expect(queuePop.locator(".qrow")).toHaveCount(1);
  const stopped = page.waitForRequest((request) => request.url().endsWith("/api/interrupt"));
  await page.getByRole("button", { name: "Stop current run" }).click();
  await stopped;
  await page.screenshot({ path: join(screenshots, "composer-a-running.png") });

  }, recover);
  await step('files panel (tree, filter, preview, phone, links)', async () => {
  // Files: lazy folders, keyboard tree, filter, immediate read-only preview, errors that stay local, mobile tabs.
  const fileRequests = [];
  page.on("request", (request) => { if (request.url().includes("/v1/files")) fileRequests.push(new URL(request.url()).pathname + new URL(request.url()).search); });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(url("?look=b"));
  assert.equal(fileRequests.length, 0, "no file request before the panel opens");
  const filesButton = page.getByRole("button", { name: "Browse files" });
  await filesButton.click();
  const filesPanel = page.locator("#files");
  await expect(filesPanel).toBeVisible();
  const tree = filesPanel.getByRole("tree", { name: "Files" });
  await expect(tree.getByRole("treeitem", { name: "src", exact: true })).toBeVisible();
  assert.deepEqual(fileRequests, ["/v1/files/tree?path="], "only the root is listed");
  await expect(tree.getByRole("treeitem", { name: "greeting.ts" })).toHaveCount(0);
  await tree.getByRole("treeitem", { name: "src", exact: true }).click();
  await expect(tree.getByRole("treeitem", { name: "greeting.ts" })).toBeVisible();
  await tree.getByRole("treeitem", { name: "greeting.ts" }).click();
  await expect(filesPanel.locator(".preview pre")).toContainText("<script>window.fileXSS = true</script>");
  assert.equal(await page.evaluate(() => window.fileXSS), undefined, "file content is text, never markup");
  await expect(filesPanel.locator(".preview")).toContainText("Read-only");
  await tree.getByRole("treeitem", { name: "large.txt" }).click();
  await expect(filesPanel.locator(".preview")).toContainText("Truncated to 64 KiB");
  assert.equal((await filesPanel.locator(".preview pre").textContent()).length, 65536);
  // Keyboard: arrows move, Right/Left expand and collapse, Home/End jump.
  await tree.getByRole("treeitem", { name: "src", exact: true }).focus();
  await page.keyboard.press("ArrowLeft");
  await expect(tree.getByRole("treeitem", { name: "src", exact: true })).toHaveAttribute("aria-expanded", "false");
  await page.keyboard.press("ArrowRight");
  await expect(tree.getByRole("treeitem", { name: "src", exact: true })).toHaveAttribute("aria-expanded", "true");
  await page.keyboard.press("ArrowDown");
  await expect(tree.getByRole("treeitem", { name: "deep", exact: true })).toBeFocused();
  await page.keyboard.press("End");
  await expect(tree.getByRole("treeitem").last()).toBeFocused();
  await page.keyboard.press("Home");
  await expect(tree.getByRole("treeitem").first()).toBeFocused();
  // Filter works on what is loaded and never lists anything new.
  const listed = fileRequests.length;
  await filesPanel.getByRole("searchbox", { name: "Filter files" }).fill("greet");
  await expect(tree.getByRole("treeitem")).toHaveCount(2);
  await filesPanel.getByRole("searchbox", { name: "Filter files" }).fill("zzz");
  await expect(filesPanel.locator(".tree-empty")).toContainText("zzz");
  await filesPanel.getByRole("searchbox", { name: "Filter files" }).fill("");
  assert.equal(fileRequests.length, listed, "filtering never requests more listings");
  await tree.getByRole("treeitem", { name: "empty", exact: true }).click();
  await expect(filesPanel.locator(".tree-empty", { hasText: "Empty folder" })).toBeVisible();
  // A failing preview stays inside the preview; the tree and the page are untouched.
  await page.route("**/v1/files?path=README.md", (route) => route.fulfill({ status: 500, contentType: "application/json", body: JSON.stringify({ error: { message: "disk exploded" } }) }));
  await tree.getByRole("treeitem", { name: "README.md" }).click();
  await expect(filesPanel.getByRole("status").filter({ hasText: "disk exploded" })).toBeVisible();
  await expect(filesPanel.locator(".preview")).toContainText("Preview unavailable");
  await expect(tree.getByRole("treeitem", { name: "src", exact: true })).toBeVisible();
  await page.unroute("**/v1/files?path=README.md");
  await page.screenshot({ path: join(screenshots, "files-b-light.png") });
  const refreshed = fileRequests.length;
  await filesPanel.getByRole("button", { name: "Refresh" }).click();
  await expect.poll(() => fileRequests.length).toBeGreaterThan(refreshed);
  await page.keyboard.press("Escape");
  await expect(filesPanel).toBeHidden();
  await expect(filesButton).toBeFocused();
  // Phone: the panel is full screen with Browse / Preview tabs.
  await page.setViewportSize({ width: 390, height: 844 });
  await phoneNav(page, "Files");
  await expect(filesPanel).toBeVisible();
  await filesPanel.getByRole("treeitem", { name: "greeting.ts" }).click(); // src stayed expanded from the desktop session
  await expect(filesPanel.locator(".preview")).toBeVisible();
  await expect(tree).toBeHidden();
  await filesPanel.getByRole("button", { name: "Browse", exact: true }).click();
  await expect(tree).toBeVisible();
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), "no overflow with the files panel on a phone");
  await page.screenshot({ path: join(screenshots, "files-mobile-b.png") });
  await filesPanel.getByRole("button", { name: "Close file browser" }).click();
  await page.setViewportSize({ width: 1440, height: 900 });

  // Transcript links open the existing read-only browser, never a file URL or a new page.
  const nestedLink = page.getByRole("link", { name: "Nested source", exact: true });
  const beforeFileLinkUrl = page.url();
  const beforeFileLinkPages = context.pages().length;
  await nestedLink.click();
  await expect(filesPanel).toBeVisible();
  await expect(filesPanel.locator(".preview pre")).toHaveText("inner file\n");
  await expect(tree.getByRole("treeitem", { name: "src", exact: true })).toHaveAttribute("aria-expanded", "true");
  await expect(tree.getByRole("treeitem", { name: "deep", exact: true })).toHaveAttribute("aria-expanded", "true");
  await expect(tree.getByRole("treeitem", { name: "inner.txt", exact: true })).toHaveAttribute("aria-selected", "true");
  await expect(tree.locator(".trow.sel")).toHaveCount(1);
  await expect(tree.getByRole("treeitem", { name: "greeting.ts", exact: true })).toHaveAttribute("aria-selected", "false");
  // Keep the pointer off tree rows: opening the panel can place an unrelated
  // row underneath it, giving that row a hover fill in the screenshot.
  await page.mouse.move(720, 20);
  await expect(tree.getByRole("treeitem", { name: "greeting.ts", exact: true })).toHaveCSS("background-color", "rgba(0, 0, 0, 0)");
  await page.screenshot({ path: join(screenshots, "file-link-desktop.png") });
  await filesPanel.getByRole("button", { name: "Close file browser" }).click();
  await expect(nestedLink).toBeFocused();
  const repeatedPreview = page.waitForRequest(request => new URL(request.url()).searchParams.get("path") === "src/deep/inner.txt" && new URL(request.url()).pathname.endsWith("/v1/files"));
  await page.keyboard.press("Enter");
  await repeatedPreview;
  await expect(filesPanel.locator(".preview pre")).toHaveText("inner file\n");
  await expect(filesPanel.getByRole("button", { name: "Close file browser" })).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(filesPanel).toBeHidden();
  await expect(nestedLink).toBeFocused();
  await page.getByRole("link", { name: "Absolute readme", exact: true }).click();
  await expect(filesPanel.locator(".preview pre")).toContainText("Small tools, thoughtful interfaces.");
  await page.getByRole("link", { name: "Local file URL", exact: true }).click();
  await expect(filesPanel.locator(".preview pre")).toHaveText("literal percent and hash filename\n");
  for (const [name, path, status] of [["Missing file", "missing.txt", 404], ["Outside file", "../escape.txt", 403]]) {
    const failure = page.waitForResponse(response => new URL(response.url()).pathname.endsWith("/v1/files") && new URL(response.url()).searchParams.get("path") === path);
    await page.getByRole("link", { name, exact: true }).click();
    assert.equal((await failure).status(), status);
    await expect(filesPanel.locator(".preview")).toContainText("Preview unavailable");
    await expect(tree.getByRole("treeitem", { name: "src", exact: true })).toBeVisible();
  }
  assert.equal(page.url(), beforeFileLinkUrl);
  assert.equal(context.pages().length, beforeFileLinkPages);
  await filesPanel.getByRole("button", { name: "Close file browser" }).click();
  await page.setViewportSize({ width: 390, height: 844 });
  await nestedLink.click();
  await expect(filesPanel.locator(".preview")).toBeVisible();
  await expect(filesPanel.locator(".preview pre")).toHaveText("inner file\n");
  await expect(tree).toBeHidden();
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), "file-link preview has no phone overflow");
  await page.screenshot({ path: join(screenshots, "file-link-mobile.png") });
  await filesPanel.getByRole("button", { name: "Close file browser" }).click();
  await page.setViewportSize({ width: 1440, height: 900 });

  }, recover);
  await step('dialogs and conversation export', async () => {
  // Long text and images open in dialogs; the conversation can be exported as Markdown.
  await page.goto(url("?look=a"));
  const failedGroup = page.locator(".grp").last();
  await failedGroup.locator(".sum").click();
  await failedGroup.locator(".tool-btn", { hasText: "durable-queue" }).click();
  await failedGroup.getByRole("button", { name: "Open in a dialog" }).click();
  const textDialog = page.getByRole("dialog", { name: /durable-queue/ });
  await expect(textDialog).toContainText("not ok 1 - fixture path");
  await textDialog.getByRole("button", { name: "Copy" }).click();
  await page.keyboard.press("Escape");
  await expect(textDialog).toBeHidden();
  const pixel = page.locator('.markdown img[alt="pixel"]');
  await pixel.scrollIntoViewIfNeeded();
  await pixel.click();
  const viewer = page.getByRole("dialog", { name: "pixel" });
  await expect(viewer.locator("img")).toBeVisible();
  await viewer.getByRole("button", { name: "Actual size" }).click();
  await viewer.getByRole("button", { name: "Zoom in" }).click();
  await expect(viewer.locator("img.zoomed")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(viewer).toBeHidden();
  await page.locator('button[aria-controls="appearance"]:visible').first().click();
  const download = page.waitForEvent("download");
  await page.getByRole("button", { name: "Export as Markdown" }).click();
  const exported = await readFile(await (await download).path(), "utf8");
  assert.match(exported, /^# /);
  for (const expected of ["## You", "remember 1", "```diff", "- [x] Inspect the workspace", "$ npm test", "![pixel]"]) assert.ok(exported.includes(expected), `export lacks ${expected}`);

  }, recover);
  await step('background tasks (Claude harness)', async () => {
  // Background tasks (Claude harness): status, output tail, polling, panel focus.
  claude = await startServer("claude", {}, "claude");
  await page.goto(`${claude}/${page_}?look=c`);
  await page.getByRole("textbox", { name: /Message/ }).fill("report the tasks");
  await expect(page.getByRole("button", { name: "Send message", exact: true })).toBeEnabled();
  await page.getByRole("textbox", { name: /Message/ }).press("Enter");
  const tasksButton = page.getByRole("button", { name: /^Background tasks/ });
  await expect(tasksButton).toBeVisible();
  await tasksButton.click();
  const tasksPanel = page.locator("#tasks");
  await expect(tasksPanel).toBeVisible();
  await expect(tasksPanel.locator(".task")).not.toHaveCount(0);
  const shell = tasksPanel.locator(".task", { hasText: "Background shell" });
  await expect(shell).toContainText("Completed");
  await shell.locator("summary").click();
  await expect(shell.locator(".task-output")).toContainText("TASK_DONE");
  await expect(tasksPanel.locator(".task", { hasText: "Failed helper" })).toContainText("Failed");
  await page.screenshot({ path: join(screenshots, "tasks-c.png") });
  await page.keyboard.press("Escape");
  await expect(tasksPanel).toBeHidden();
  await expect(tasksButton).toBeFocused();

  }, recover);
  await step('header entry points and menus', async () => {
  // Same layout checks on whichever harness is available. Only a harness that reports background tasks (Claude) shows the
  // Tasks entry, so the expected entry points follow the harness capability instead of requiring Claude.
  const header = claude ?? (idle ??= await startServer("idle", { FAKE_CODEX_MODELS: "1" }));
  const withTasks = Boolean(claude);
  // Every look puts the same entry points in the same places: header actions top right on desktop, bottom bar on phones.
  const orders = {};
  for (const look of ["a", "b", "c"]) {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto(`${header}/${page_}?look=${look}`);
    await expect(page.locator("header.top .actions")).toBeVisible();
    const desktop = await page.evaluate(() => {
      const header = document.querySelector("header.top").getBoundingClientRect();
      const buttons = [...document.querySelectorAll("header.top .actions button")].filter((b) => b.offsetParent !== null);
      return { labels: buttons.map((b) => b.getAttribute("aria-label")), inside: buttons.every((b) => { const r = b.getBoundingClientRect(); return r.top >= header.top && r.bottom <= header.bottom + 1 && r.right <= header.right; }),
        ascending: buttons.every((b, i) => i === 0 || b.getBoundingClientRect().x > buttons[i - 1].getBoundingClientRect().x), gapToEdge: Math.round(header.right - buttons.at(-1).getBoundingClientRect().right), rail: Boolean(document.querySelector(".rail")) };
    });
    assert.deepEqual(desktop.labels, [...(withTasks ? ["Background tasks, 0 running"] : []), "Browse files", "Appearance", "Toggle theme"], `header actions in ${look}`);
    assert.ok(desktop.inside && desktop.ascending, `header actions are left to right inside the header in ${look}`);
    // C aligns its header to its centred content column, so only the full-width looks must hug the window edge.
    if (look !== "c") assert.ok(desktop.gapToEdge >= 0 && desktop.gapToEdge <= 24, `header actions hug the right edge in ${look} (${desktop.gapToEdge}px)`);
    assert.equal(desktop.rail, false, "no side rail in any look");
    orders[look] = desktop.labels.join("|");
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto(`${header}/${page_}?look=${look}&theme=light`);
    const menuButton = page.getByRole("button", { name: "Menu", exact: true });
    await expect(menuButton).toBeVisible();
    assert.equal(await page.locator(".tabbar").count(), 0, `no bottom bar in ${look}`);
    assert.equal(await page.evaluate(() => [...document.querySelectorAll("header.top .actions button")].filter((b) => b.offsetParent !== null).length), 1, `the Menu button is the only header action on a phone in ${look}`);
    // Keyboard and aria: Enter opens, the first item takes focus, arrows move, Esc closes and returns focus.
    await menuButton.focus();
    await expect(menuButton).toHaveAttribute("aria-haspopup", "menu");
    await expect(menuButton).toHaveAttribute("aria-expanded", "false");
    await page.keyboard.press("Enter");
    await expect(menuButton).toHaveAttribute("aria-expanded", "true");
    const items = page.getByRole("menu", { name: "Menu" }).getByRole("menuitem");
    await expect(items).toHaveText(["Files", ...(withTasks ? ["Tasks"] : []), "Appearance", "Theme"]);
    await expect(items.first()).toBeFocused();
    await page.keyboard.press("ArrowDown");
    await expect(items.nth(1)).toBeFocused();
    await page.keyboard.press("End");
    await expect(items.last()).toBeFocused();
    await page.screenshot({ path: join(screenshots, `phone-menu-open-${look}-light.png`) });
    await page.keyboard.press("Escape");
    await expect(page.getByRole("menu")).toHaveCount(0);
    await expect(menuButton).toBeFocused();
    const bar = await page.evaluate(() => { const r = document.querySelector("header.top").getBoundingClientRect(), m = document.querySelector(".nav-btn").getBoundingClientRect(); return m.right <= r.right && m.top >= r.top && m.bottom <= r.bottom; });
    assert.ok(bar, `the Menu button sits inside the top bar in ${look}`);
    // Theme and Appearance work from the menu in both themes.
    const before = await page.getAttribute("html", "data-theme");
    await phoneNav(page, "Theme");
    assert.notEqual(await page.getAttribute("html", "data-theme"), before, `Theme from the menu toggles in ${look}`);
    await menuButton.click();
    await page.screenshot({ path: join(screenshots, `phone-menu-open-${look}-dark.png`) });
    await page.keyboard.press("Escape");
    await phoneNav(page, "Appearance");
    await expect(page.getByRole("dialog", { name: "Appearance" })).toBeVisible();
    await page.keyboard.press("Escape");
    await page.screenshot({ path: join(screenshots, `phone-menu-${look}.png`) });
  }
  assert.equal(new Set(Object.values(orders)).size, 1, "all looks order the header actions identically");

  }, recover);
  await step('idle is still', async () => {
  const idleBase = claude ?? (idle ??= await startServer("idle", { FAKE_CODEX_MODELS: "1" }));
  // Idle is still: the status block never animates unless the agent is working; reduced motion removes the working animation too.
  const dotAnimation = () => page.evaluate(() => getComputedStyle(document.querySelector(".status .dot")).animationName);
  for (const look of ["a", "b", "c"]) {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto(`${idleBase}/${page_}?look=${look}`);
    await expect(page.locator(".status-idle")).toBeVisible();
    assert.equal(await dotAnimation(), "none", `idle status block does not animate in ${look}`);
    assert.equal(await page.evaluate(() => { const h = document.querySelector(".v-empty h2"); return h ? getComputedStyle(h, "::after").animationName : "none"; }), "none", `the empty-state cursor is steady in ${look}`);
    await page.goto(`${live}/${page_}?look=${look}`);
    await expect(page.locator(".status-working")).toBeVisible();
    if (look !== "b") assert.notEqual(await dotAnimation(), "none", `the working status block still animates in ${look}`);
    await page.emulateMedia({ reducedMotion: "reduce" });
    assert.ok(await page.evaluate(() => { const style = getComputedStyle(document.querySelector(".status .dot")); return style.animationName === "none" || parseFloat(style.animationDuration) <= 0.001; }), `reduced motion stops the working animation in ${look}`);
    await page.emulateMedia({ reducedMotion: "no-preference" });
  }
  await page.setViewportSize({ width: 1440, height: 900 });

  }, recover);
  await step('finished goal notice', async () => {
  // A finished goal leaves a notice in the conversation (the fake completes a goal when its first turn starts).
  done = await startServer("done", {});
  await page.goto(`${done}/${page_}?look=c`);
  const doneStrip = page.getByRole("toolbar", { name: "Run status" });
  await doneStrip.getByRole("button", { name: "Set goal" }).click();
  await page.getByRole("dialog", { name: "Set goal" }).getByLabel("Objective").fill("Finish the migration");
  await page.getByRole("dialog", { name: "Set goal" }).getByRole("button", { name: "Save goal" }).click();
  await expect(page.locator(".goal-done")).toContainText("Goal complete: Finish the migration");

  }, recover);
  await step('Chinese interface', async () => {
  // Chinese interface: dictionary strings, interpolation, English fallback for a missing key, and no raw keys on screen.
  await page.goto(url("?look=b&lang=zh"));
  await expect(page.locator("html")).toHaveAttribute("lang", "zh-Hans");
  await expect(page.getByRole("textbox", { name: "给 Codex 发消息" })).toBeVisible();
  await expect(page.locator(".status-t")).toHaveText("空闲");
  await expect(page.locator(".grp .t").first()).toHaveText("活动");
  await page.getByRole("button", { name: /^目标/ }).click();
  const chineseGoalPop = page.getByRole("region", { name: "目标", exact: true });
  await expect(chineseGoalPop).toContainText("token 上限：无限制 · 轮次上限：无限制");
  await page.screenshot({ path: join(screenshots, "goal-unlimited-zh-desktop.png") });
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(chineseGoalPop).toContainText("无限制");
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  await page.screenshot({ path: join(screenshots, "goal-unlimited-zh-390px.png") });
  await page.setViewportSize({ width: 1440, height: 900 });
  await chineseGoalPop.getByRole("button", { name: "编辑目标", exact: true }).click();
  const chineseGoal = page.getByRole("dialog", { name: "编辑目标" });
  await expect(chineseGoal.getByLabel("token 预算(可选)")).toHaveAttribute("placeholder", "无限制");
  await expect(chineseGoal).toContainText("留空表示无限制");
  await chineseGoal.getByRole("button", { name: "取消", exact: true }).click();
  assert.equal(await page.evaluate(() => /\b(?:composer|status|activity|goal|plan|queue|tool|thread|appearance)\.[a-zA-Z.]+\b/.test(document.body.innerText)), false, "no raw message keys on screen");

  }, recover);
  await step('narrow screen', async () => {
  // Narrow screen: bottom bar, no horizontal overflow, in every look.
  for (const look of ["a", "b", "c"]) {
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto(url(`?look=${look}`));
    await expect(page.getByRole("button", { name: "Menu", exact: true })).toBeVisible();
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), `no horizontal overflow in look ${look}`);
    await page.getByRole("toolbar", { name: "Run status" }).getByRole("button", { name: /Plan/ }).click();
    const sheet = await page.locator("#pop-plan").boundingBox();
    assert.ok(sheet && sheet.height <= 844 * 0.4 + 1 && Math.abs(sheet.y + sheet.height - 844) < 2, `plan popover is a bottom sheet of at most 40vh in look ${look}`);
    await page.screenshot({ path: join(screenshots, `mobile-${look}-plan.png`) });
    await page.keyboard.press("Escape");

    await page.screenshot({ path: join(screenshots, `mobile-${look}.png`) });
  }
  }, recover);
  await step('CSP violations and page errors', async () => {
  // The only expected violations are the hostile fixture's two inline handlers (img onerror, svg onload): DOMPurify's inert
  // parse trips script-src-attr and then strips them. Anything else, or a different count, is a real policy failure.
  const violations = await page.evaluate(() => window.__violations);
  assert.deepEqual(violations.map((line) => line.split(" ").slice(0, 2).join(" ")), ["script-src-attr inline", "script-src-attr inline"], violations.join("\n"));
  assert.deepEqual(errors.filter((message) => !/inline event handler|status of 500/.test(message)), []); // the 500 is the injected preview failure
  }, recover);
  await step("runtime controls (mocked API)", () => testRuntimeControlsUi(browser, screenshots, page_), recover);
  await step("bounded history pages (mocked API)", () => testHistoryPages(browser, base, screenshots, page_), recover);
  await step("image attachments", async () => { imageBase ??= await startServer("images", { FAKE_CODEX_IMAGES: "1" }); await testImagesUi(browser, imageBase, screenshots, page_); }, recover);
  await step("file download", async () => { imageBase ??= await startServer("images", { FAKE_CODEX_IMAGES: "1" }); await testFilesDownloadUi(browser, imageBase, screenshots, page_); }, recover);
  await step("mobile composer", async () => { imageBase ??= await startServer("images", { FAKE_CODEX_IMAGES: "1" }); await testMobileComposer(browser, imageBase, screenshots, page_); }, recover);
  if (steps.results.length) { const failed = steps.summary(); if (failed) process.exitCode = 1; }
  else console.log(`React UI browser checks passed. Screenshots: ${screenshots}`);
} finally {
  await browser?.close();
  for (const { server, cwd, privateRoot, controller } of servers) {
    const exited = server.exitCode === null ? new Promise((resolve) => server.once("exit", resolve)) : Promise.resolve();
    server.kill("SIGTERM");
    await exited;
    await controller.close();
    await rm(cwd, { recursive: true, force: true });
    await rm(privateRoot, { recursive: true, force: true });
  }
}
