import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, mkdir, writeFile, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { chromium, expect } from "@playwright/test";
import { hermeticEnv, localFetch as fetch } from "./hermetic.mjs";

// The React UI (public/ui.html) against the real launcher server and a fake Codex: no inference, no Treer mutations.
// Usage: node tests/browser-ui.mjs [--page ui.html]   (the page name changes when the UI becomes the default)
const app = resolve(import.meta.dirname, "..");
const pageArgIndex = process.argv.indexOf("--page");
const page_ = pageArgIndex > 0 ? process.argv[pageArgIndex + 1] : "ui.html";
const cwd = await mkdtemp(join(tmpdir(), "ui-browser-"));
const screenshots = process.env.UI_SCREENSHOTS || await mkdtemp(join(tmpdir(), "ui-shots-"));
await mkdir(screenshots, { recursive: true });
await mkdir(join(cwd, "src"));
const env = hermeticEnv({ TREER_AGENT_ID: "ag_browser_ui", AGENT_UI_INSTANCE_ID: "browser-ui", CODEX_UI_CWD: cwd, CODEX_UI_PUBLIC_DIR: join(app, "public"), CODEX_UI_PORT: "0", CODEX_UI_PORT_FILE: join(cwd, "port"),
  CODEX_BIN: join(app, "tests/fake-codex.mjs"), FAKE_CODEX_UX: "1" });
const server = spawn(join(app, "node_modules/.bin/tsx"), [join(app, "src/index.ts")], { env, stdio: "pipe" });
let logs = "";
server.stderr.on("data", (chunk) => { logs += chunk; });
server.stdout.on("data", (chunk) => { logs += chunk; });
let browser;
try {
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
  const url = (query = "") => `${base}/${page_}${query}`;

  // Headers: the React page gets the strict policy, the legacy page is unchanged.
  const csp = (await fetch(url())).headers.get("content-security-policy") ?? "";
  assert.match(csp, /script-src 'self'/);
  assert.match(csp, /style-src 'self'(?:;|$)/);
  assert.doesNotMatch(csp, /unsafe-inline|unsafe-eval/);
  for (const directive of ["object-src 'none'", "base-uri 'self'", "form-action 'none'"]) assert.ok(csp.includes(directive), `CSP lacks ${directive}`);
  if (page_ === "ui.html") assert.match((await fetch(base + "/")).headers.get("content-security-policy") ?? "", /style-src 'self' 'unsafe-inline'/);

  browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: "light", extraHTTPHeaders: { "x-treer-interface-source": "proxy-ui", "x-treer-agent-id": "ag_browser_ui", "x-treer-interface-instance": "browser-ui" } });
  await context.addInitScript(() => { window.__violations = []; document.addEventListener("securitypolicyviolation", (event) => window.__violations.push(`${event.violatedDirective} ${event.blockedURI}`)); });
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

  // Narrow screen: bottom bar, no horizontal overflow, in every look.
  for (const look of ["a", "b", "c"]) {
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto(url(`?look=${look}`));
    await expect(page.locator(".tabbar")).toBeVisible();
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), `no horizontal overflow in look ${look}`);
    await page.screenshot({ path: join(screenshots, `mobile-${look}.png`) });
  }
  assert.deepEqual(await page.evaluate(() => window.__violations), [], "no CSP violations");
  assert.deepEqual(errors, []);
  console.log(`React UI browser checks passed. Screenshots: ${screenshots}`);
} finally {
  await browser?.close();
  const exited = server.exitCode === null ? new Promise((resolve) => server.once("exit", resolve)) : Promise.resolve();
  server.kill("SIGTERM");
  await exited;
  await rm(cwd, { recursive: true, force: true });
}
