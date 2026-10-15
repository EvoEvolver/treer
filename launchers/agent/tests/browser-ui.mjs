import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdtemp, mkdir, writeFile, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { chromium, expect } from "@playwright/test";
import { hermeticEnv, localFetch as fetch } from "./hermetic.mjs";
import { testRuntimeControlsUi } from "./browser-controls-ui.mjs";

// The React UI (public/ui.html) against the real launcher server and a fake Codex: no inference, no Treer mutations.
// Usage: node tests/browser-ui.mjs [--page index.html]
const app = resolve(import.meta.dirname, "..");
const pageArgIndex = process.argv.indexOf("--page");
const page_ = pageArgIndex > 0 ? process.argv[pageArgIndex + 1] : "index.html";
const screenshots = process.env.UI_SCREENSHOTS || process.env.CODEX_UI_SCREENSHOTS || await mkdtemp(join(tmpdir(), "ui-shots-"));
await mkdir(screenshots, { recursive: true });
const servers = [];
async function startServer(name, extra, harness = "codex") {
  const cwd = await mkdtemp(join(tmpdir(), `ui-browser-${name}-`));
  await mkdir(join(cwd, "src/deep"), { recursive: true });
  await mkdir(join(cwd, "empty"));
  await writeFile(join(cwd, "src/greeting.ts"), "export const greeting = 'Hello, Treer';\n// <script>window.fileXSS = true</script>\n");
  await writeFile(join(cwd, "src/deep/inner.txt"), "inner file\n");
  await writeFile(join(cwd, "README.md"), "# Workspace\nSmall tools, thoughtful interfaces.\n");
  await writeFile(join(cwd, "large.txt"), "x".repeat(65537));
  // The unique marker identifies exactly the processes this test started; they are stopped through their own handles only.
  const common = { UI_DEV_FIXTURE: randomUUID(), TREER_AGENT_ID: "ag_browser_ui", AGENT_UI_INSTANCE_ID: "browser-ui", CODEX_UI_PUBLIC_DIR: join(app, "public"), CODEX_UI_PORT: "0" };
  const env = hermeticEnv(harness === "claude"
    ? { ...common, AGENT_UI_HARNESS: "claude", AGENT_UI_CWD: cwd, AGENT_UI_PORT: "0", AGENT_UI_PORT_FILE: join(cwd, "port"), AGENT_UI_PUBLIC_DIR: join(app, "public"), CLAUDE_BIN: join(app, "tests/fake-claude.mjs"), ...extra }
    : { ...common, CODEX_UI_CWD: cwd, CODEX_UI_PORT_FILE: join(cwd, "port"), CODEX_BIN: join(app, "tests/fake-codex.mjs"), FAKE_CODEX_UX: "1", ...extra });
  const server = spawn(join(app, "node_modules/.bin/tsx"), [join(app, "src/index.ts")], { env, stdio: "pipe" });
  let logs = "";
  server.stderr.on("data", (chunk) => { logs += chunk; });
  server.stdout.on("data", (chunk) => { logs += chunk; });
  servers.push({ server, cwd });
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
let browser;
try {
  const base = await startServer("rich", { FAKE_CODEX_RICH: "1", FAKE_CODEX_MANY: "80", FAKE_CODEX_MODELS: "1", FAKE_CODEX_GOAL_ACTIVE: "1" });
  const url = (query = "") => `${base}/${page_}${query}`;

  // Headers: the React page gets the strict policy, the legacy page is unchanged.
  const csp = (await fetch(url())).headers.get("content-security-policy") ?? "";
  const asset = await fetch(url().replace(/index\.html$/, "ui.js"));
  assert.equal(asset.headers.get("x-content-type-options"), "nosniff");
  assert.equal(asset.headers.get("referrer-policy"), "no-referrer");
  assert.match(csp, /script-src 'self'/);
  assert.match(csp, /style-src 'self'(?:;|$)/);
  assert.doesNotMatch(csp, /unsafe-inline|unsafe-eval/);
  for (const directive of ["object-src 'none'", "base-uri 'self'", "form-action 'none'"]) assert.ok(csp.includes(directive), `CSP lacks ${directive}`);
  assert.match((await fetch(base + "/")).headers.get("content-security-policy") ?? "", /style-src 'self'(?:;|$)/, "the root serves the same page under the same policy");

  browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: "light", extraHTTPHeaders: { "x-treer-interface-source": "proxy-ui", "x-treer-agent-id": "ag_browser_ui", "x-treer-interface-instance": "browser-ui" } });
  await context.addInitScript(() => { window.__violations = []; document.addEventListener("securitypolicyviolation", (event) => window.__violations.push(`${event.violatedDirective} ${event.blockedURI} ${event.sample} ${event.sourceFile}:${event.lineNumber}`)); });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => { if (message.type() === "error") errors.push(message.text()); });

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
  await page.goto(`${base}/api/health`); // a same-origin page without the UI policy; setContent would otherwise inherit it
  await page.setContent(`<iframe title="Agent" src="${url("?theme=light")}" style="width:100%;height:700px"></iframe>`);
  const frame = page.frameLocator("iframe");
  await expect(frame.locator("html")).toHaveAttribute("data-theme", "light");
  await page.evaluate(() => document.querySelector("iframe").contentWindow.postMessage({ type: "treer:embed-theme", theme: "dark" }, "*"));
  await expect(frame.locator("html")).toHaveAttribute("data-theme", "dark", "parent message");

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
  // Model and effort through the settings API.
  await page.getByRole("button", { name: /^Model/ }).click();
  const modelPop = page.getByRole("region", { name: "Model and effort" });
  const settings = page.waitForRequest((request) => request.url().endsWith("/api/settings") && request.method() === "POST");

  await modelPop.locator("select").first().selectOption("gpt-b");
  assert.equal(JSON.parse((await settings).postData()).model, "gpt-b");
  await expect(page.getByRole("button", { name: /^Model GPT B/ })).toBeVisible();
  await page.keyboard.press("Escape");
  await page.screenshot({ path: join(screenshots, "composer-b-light.png") });

  // A running agent: separate Steer and Queue buttons, queued messages, interrupt.
  const live = await startServer("live", { FAKE_CODEX_LIVE: "1", FAKE_CODEX_MODELS: "1" });
  await page.goto(`${live}/${page_}?look=a`);
  const liveBox = page.getByRole("textbox", { name: /Message/ });
  await expect(page.getByRole("button", { name: "Stop current run" })).toBeEnabled();
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
  await page.getByRole("button", { name: "Files", exact: true }).click();
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

  // Background tasks (Claude harness): status, output tail, polling, panel focus.
  const claude = await startServer("claude", {}, "claude");
  await page.goto(`${claude}/${page_}?look=c`);
  await page.getByRole("textbox", { name: /Message/ }).fill("report the tasks");
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

  // A finished goal leaves a notice in the conversation (the fake completes a goal when its first turn starts).
  const done = await startServer("done", {});
  await page.goto(`${done}/${page_}?look=c`);
  const doneStrip = page.getByRole("toolbar", { name: "Run status" });
  await doneStrip.getByRole("button", { name: "Set goal" }).click();
  await page.getByRole("dialog", { name: "Set goal" }).getByLabel("Objective").fill("Finish the migration");
  await page.getByRole("dialog", { name: "Set goal" }).getByRole("button", { name: "Save goal" }).click();
  await expect(page.locator(".goal-done")).toContainText("Goal complete: Finish the migration");

  // Chinese interface: dictionary strings, interpolation, English fallback for a missing key, and no raw keys on screen.
  await page.goto(url("?look=b&lang=zh"));
  await expect(page.locator("html")).toHaveAttribute("lang", "zh-Hans");
  await expect(page.getByRole("textbox", { name: "给 Codex 发消息" })).toBeVisible();
  await expect(page.locator(".status-t")).toHaveText("空闲");
  await expect(page.locator(".grp .t").first()).toHaveText("活动");
  assert.equal(await page.evaluate(() => /\b(?:composer|status|activity|goal|plan|queue|tool|thread|appearance)\.[a-zA-Z.]+\b/.test(document.body.innerText)), false, "no raw message keys on screen");

  // Narrow screen: bottom bar, no horizontal overflow, in every look.
  for (const look of ["a", "b", "c"]) {
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto(url(`?look=${look}`));
    await expect(page.locator(".tabbar")).toBeVisible();
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), `no horizontal overflow in look ${look}`);
    await page.getByRole("toolbar", { name: "Run status" }).getByRole("button", { name: /Plan/ }).click();
    const sheet = await page.locator("#pop-plan").boundingBox();
    assert.ok(sheet && sheet.height <= 844 * 0.4 + 1 && Math.abs(sheet.y + sheet.height - 844) < 2, `plan popover is a bottom sheet of at most 40vh in look ${look}`);
    await page.screenshot({ path: join(screenshots, `mobile-${look}-plan.png`) });
    await page.keyboard.press("Escape");

    await page.screenshot({ path: join(screenshots, `mobile-${look}.png`) });
  }
  // The only expected violations are the hostile fixture's two inline handlers (img onerror, svg onload): DOMPurify's inert
  // parse trips script-src-attr and then strips them. Anything else, or a different count, is a real policy failure.
  const violations = await page.evaluate(() => window.__violations);
  assert.deepEqual(violations.map((line) => line.split(" ").slice(0, 2).join(" ")), ["script-src-attr inline", "script-src-attr inline"], violations.join("\n"));
  assert.deepEqual(errors.filter((message) => !/inline event handler|status of 500/.test(message)), []); // the 500 is the injected preview failure
  await testRuntimeControlsUi(browser, screenshots, page_);
  console.log(`React UI browser checks passed. Screenshots: ${screenshots}`);
} finally {
  await browser?.close();
  for (const { server, cwd } of servers) {
    const exited = server.exitCode === null ? new Promise((resolve) => server.once("exit", resolve)) : Promise.resolve();
    server.kill("SIGTERM");
    await exited;
    await rm(cwd, { recursive: true, force: true });
  }
}
